-- Holds and available balance (K-05).
--
-- A hold reserves an amount against an account without moving its posted balance. Available
-- balance is posted balance, minus active holds, plus the arranged overdraft. Every debit
-- authorises against it at the account's home, because post_leg (replaced below) reads the active
-- holds in the same statement that writes the entry, under the account's row lock.
--
-- A hold moves through one transition, from active to released, converted or expired, and each
-- transition can be repeated safely: a repeat returns the hold as it is. A hold that already left
-- active by a different route is refused, with SQLSTATE LE008. Others:
--   LE001  insufficient funds (placing a hold, or a debit)    LE002  fenced
--   LE003  the hold or command was used before with different details
--   LE004  unknown account                                    LE009  unknown hold
-- An active hold counts until a sweep expires it, so expiry is explicit and never a matter of
-- when a balance happens to be read.

create or replace function post_leg(
  p_tenant       text,
  p_journal      text,
  p_account      text,
  p_leg          smallint,
  p_kind         text,
  p_amount       bigint,
  p_currency     text,
  p_counterparty text,
  p_origin       text,
  p_epoch        bigint,
  p_entry        text
) returns table (
  entry_id  text,
  epoch     bigint,
  seq       bigint,
  balance   bigint,
  watermark bigint,
  replayed  boolean
) language plpgsql
  -- Tables are found in the schema the migrator was working in, wherever the caller points.
  set search_path from current
as $$
declare
  acc        account%rowtype;
  existing   entry%rowtype;
  latest     bigint;
  next_seq_n bigint;
  signed     bigint;
  position_n bigint;
  held       bigint;
begin
  if p_amount is null or p_amount <= 0 then
    raise exception 'the amount of a leg must be positive' using errcode = 'LE006';
  end if;
  if p_kind not in ('debit', 'credit') then
    raise exception 'a leg is a debit or a credit' using errcode = '22023';
  end if;

  select * into acc from account a
   where a.tenant_id = p_tenant::tenant_ref and a.id = p_account::account_ref
   for update;
  if not found then
    raise exception 'unknown account %', p_account using errcode = 'LE004';
  end if;
  if acc.currency <> p_currency::currency_code then
    raise exception 'account % is in %, not %', p_account, acc.currency, p_currency using errcode = 'LE005';
  end if;

  signed := case p_kind when 'debit' then -p_amount else p_amount end;

  select * into existing from entry e
   where e.tenant_id = acc.tenant_id and e.journal_id = p_journal::journal_ref
     and e.account_id = acc.id and e.leg = p_leg;
  if found then
    if existing.amount <> signed or existing.kind <> p_kind then
      raise exception 'journal % leg % on % was posted as % %, not % %',
        p_journal, p_leg, p_account, existing.kind, abs(existing.amount), p_kind, p_amount
        using errcode = 'LE003';
    end if;
    return query
      select existing.id::text, existing.epoch, existing.seq, acc.balance,
             (select o.position from outbox o where o.tenant_id = acc.tenant_id and o.entry_id = existing.id),
             true;
    return;
  end if;

  select coalesce(max(f.epoch), 0) into latest from epoch_fence f
   where f.tenant_id = acc.tenant_id and f.account_id = acc.id;
  if p_epoch is distinct from latest then
    raise exception 'fenced: % is written at epoch %, but the database is at epoch %', p_account, p_epoch, latest
      using errcode = 'LE002';
  end if;

  -- Every debit authorises against available balance: posted, minus active holds, plus the
  -- arranged overdraft. A hold being converted is no longer active, so it never blocks itself.
  select coalesce(sum(h.amount), 0) into held from hold h
   where h.tenant_id = acc.tenant_id and h.account_id = acc.id and h.state = 'active';
  if p_kind = 'debit' and not acc.allow_negative and acc.balance - held - p_amount < -acc.overdraft_limit then
    raise exception 'insufficient funds on %: balance %, held %, debit %, overdraft %',
      p_account, acc.balance, held, p_amount, acc.overdraft_limit using errcode = 'LE001';
  end if;

  select coalesce(max(e.seq), 0) + 1 into next_seq_n from entry e
   where e.tenant_id = acc.tenant_id and e.account_id = acc.id and e.epoch = p_epoch;

  insert into entry (tenant_id, id, journal_id, account_id, epoch, seq, leg, kind, amount, counterparty, origin)
  values (acc.tenant_id, p_entry::entry_ref, p_journal::journal_ref, acc.id, p_epoch, next_seq_n,
          p_leg, p_kind, signed, p_counterparty::account_ref, p_origin::node_name);

  update account a set balance = a.balance + signed, next_seq = next_seq_n + 1
   where a.tenant_id = acc.tenant_id and a.id = acc.id
   returning a.balance into acc.balance;

  insert into outbox (tenant_id, entry_id, account_id, epoch, seq)
  values (acc.tenant_id, p_entry::entry_ref, acc.id, p_epoch, next_seq_n)
  returning outbox.position into position_n;

  return query select p_entry, p_epoch, next_seq_n, acc.balance, position_n, false;
end $$;

-- Available balance at the account's home. It is only meant for accounts that cannot overdraw
-- freely: an account that allows a negative balance is not refused by it.
create function available_balance(p_tenant text, p_account text) returns bigint
  language sql stable
  set search_path from current
as $$
  select a.balance - coalesce((select sum(h.amount) from hold h
                                where h.tenant_id = a.tenant_id and h.account_id = a.id and h.state = 'active'), 0)
         + a.overdraft_limit
    from account a
   where a.tenant_id = p_tenant::tenant_ref and a.id = p_account::account_ref
$$;

create function hold_check_epoch(p_tenant text, p_account text, p_epoch bigint) returns void
  language plpgsql
  set search_path from current
as $$
declare latest bigint;
begin
  select coalesce(max(f.epoch), 0) into latest from epoch_fence f
   where f.tenant_id = p_tenant::tenant_ref and f.account_id = p_account::account_ref;
  if p_epoch is distinct from latest then
    raise exception 'fenced: % is written at epoch %, but the database is at epoch %', p_account, p_epoch, latest
      using errcode = 'LE002';
  end if;
end $$;

-- Places a hold, after checking the account can afford it. A repeat of the same hold, by id or
-- by command, returns it as it is.
create function place_hold(
  p_tenant text, p_hold text, p_account text, p_amount bigint,
  p_command text, p_expires_at text, p_epoch bigint
) returns table (state text, amount bigint, journal_id text, replayed boolean)
  language plpgsql
  set search_path from current
as $$
declare
  acc account%rowtype;
  h   hold%rowtype;
begin
  if p_amount is null or p_amount <= 0 then
    raise exception 'the amount of a hold must be positive' using errcode = 'LE006';
  end if;
  select * into acc from account a
   where a.tenant_id = p_tenant::tenant_ref and a.id = p_account::account_ref for update;
  if not found then
    raise exception 'unknown account %', p_account using errcode = 'LE004';
  end if;

  select * into h from hold x where x.tenant_id = acc.tenant_id and x.id = p_hold::hold_ref;
  if not found and p_command is not null then
    select * into h from hold x where x.tenant_id = acc.tenant_id and x.command_id = p_command::command_ref;
  end if;
  if found then
    if h.id <> p_hold::hold_ref or h.account_id <> acc.id or h.amount <> p_amount then
      raise exception 'hold % or command % was used before for a different hold', p_hold, p_command
        using errcode = 'LE003';
    end if;
    return query select h.state, h.amount, h.journal_id::text, true;
    return;
  end if;

  perform hold_check_epoch(p_tenant, p_account, p_epoch);
  if not acc.allow_negative and available_balance(p_tenant, p_account) - p_amount < 0 then
    raise exception 'insufficient funds on %: available %, hold %',
      p_account, available_balance(p_tenant, p_account), p_amount using errcode = 'LE001';
  end if;
  insert into hold (tenant_id, id, account_id, amount, expires_at, command_id)
  values (acc.tenant_id, p_hold::hold_ref, acc.id, p_amount, p_expires_at::timestamptz, p_command::command_ref);
  return query select 'active'::text, p_amount, null::text, false;
end $$;

-- Releases an active hold. A repeat on a released hold changes nothing.
create function release_hold(p_tenant text, p_hold text, p_epoch bigint)
  returns table (state text, amount bigint, journal_id text, replayed boolean)
  language plpgsql
  set search_path from current
as $$
declare
  h hold%rowtype;
begin
  select * into h from hold x where x.tenant_id = p_tenant::tenant_ref and x.id = p_hold::hold_ref;
  if not found then
    raise exception 'unknown hold %', p_hold using errcode = 'LE009';
  end if;
  perform 1 from account a where a.tenant_id = h.tenant_id and a.id = h.account_id for update;
  select * into h from hold x where x.tenant_id = h.tenant_id and x.id = h.id for update;
  if h.state = 'released' then
    return query select h.state, h.amount, h.journal_id::text, true;
    return;
  end if;
  if h.state <> 'active' then
    raise exception 'hold % is %, so it cannot be released', p_hold, h.state using errcode = 'LE008';
  end if;
  perform hold_check_epoch(p_tenant, h.account_id::text, p_epoch);
  update hold x set state = 'released' where x.tenant_id = h.tenant_id and x.id = h.id;
  return query select 'released'::text, h.amount, null::text, false;
end $$;

-- Converts an active hold into the debit of a journal, in one call: the hold stops reserving,
-- and the debit authorises against what is available without it. If the debit is refused, the
-- hold is still active. A repeat with the same journal returns what was written the first time.
-- The amount is the hold's unless the caller gives another, as card clearing may.
create function convert_hold(
  p_tenant text, p_hold text, p_journal text, p_amount bigint,
  p_counterparty text, p_origin text, p_epoch bigint, p_entry text
) returns table (state text, entry_id text, epoch bigint, seq bigint, balance bigint, watermark bigint, replayed boolean)
  language plpgsql
  set search_path from current
as $$
declare
  h   hold%rowtype;
  acc account%rowtype;
  amt bigint;
  leg record;
begin
  select * into h from hold x where x.tenant_id = p_tenant::tenant_ref and x.id = p_hold::hold_ref;
  if not found then
    raise exception 'unknown hold %', p_hold using errcode = 'LE009';
  end if;
  select * into acc from account a where a.tenant_id = h.tenant_id and a.id = h.account_id for update;
  select * into h from hold x where x.tenant_id = h.tenant_id and x.id = h.id for update;
  amt := coalesce(p_amount, h.amount);
  if h.state = 'converted' then
    if h.journal_id is distinct from p_journal::journal_ref then
      raise exception 'hold % was converted by journal %, not %', p_hold, h.journal_id, p_journal using errcode = 'LE008';
    end if;
  elsif h.state = 'active' then
    update hold x set state = 'converted', journal_id = p_journal::journal_ref
     where x.tenant_id = h.tenant_id and x.id = h.id;
  else
    raise exception 'hold % is %, so it cannot be converted', p_hold, h.state using errcode = 'LE008';
  end if;
  select * into leg from post_leg(p_tenant, p_journal, acc.id::text, 1::smallint, 'debit', amt,
                                  acc.currency::text, p_counterparty, p_origin, p_epoch, p_entry);
  return query select 'converted'::text, leg.entry_id, leg.epoch, leg.seq, leg.balance, leg.watermark, leg.replayed;
end $$;

-- Expires every active hold whose time has passed, and returns the ones it expired. A repeat
-- returns none. p_now is the time to judge by, text in RFC 3339, so a sweep can be replayed.
create function expire_holds(p_tenant text, p_now text) returns table (hold_id text)
  language plpgsql
  set search_path from current
as $$
declare
  h record;
begin
  for h in
    select x.id, x.account_id from hold x
     where x.tenant_id = p_tenant::tenant_ref and x.state = 'active'
       and x.expires_at is not null and x.expires_at <= p_now::timestamptz
     order by x.account_id, x.id
  loop
    perform 1 from account a where a.tenant_id = p_tenant::tenant_ref and a.id = h.account_id for update;
    update hold x set state = 'expired'
     where x.tenant_id = p_tenant::tenant_ref and x.id = h.id and x.state = 'active';
    if found then
      hold_id := h.id::text;
      return next;
    end if;
  end loop;
end $$;
