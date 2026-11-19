-- The write path's one SQL call: post one leg of a journal at the account's home.
--
-- A leg is one entry. In one statement, inside the caller's transaction, this function
--   1. locks the account row (so legs on one account run one at a time),
--   2. answers a repeat of the same leg from the entry already written, and refuses a repeat
--      that changes the amount or the side,
--   3. fences: the writer's epoch must be the latest epoch the database knows for the account,
--   4. checks the account can afford a debit (balance plus arranged overdraft),
--   5. writes the entry at the next sequence number of the epoch,
--   6. updates the balance projection, and
--   7. writes the outbox row, and returns its position, which is the cell's watermark.
-- Holds join the funds check in a later migration (K-05).
--
-- Parameters are plain text and numbers, not the schema's domains, so any client can send them.
-- The table's own domains and checks still apply to what is written. The amount is positive
-- here: the function applies the entry sign convention (a debit is stored negative).
--
-- Failures carry a SQLSTATE of their own, in class LE, so callers act on why and not on text:
--   LE001  insufficient funds
--   LE002  fenced: the writer's epoch is not the account's latest epoch
--   LE003  the leg was posted before with a different amount or side
--   LE004  unknown account
--   LE005  the amount's currency is not the account's
--   LE006  the amount must be positive
-- Messages name ids and numbers only, never personal data.
create function post_leg(
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

  if p_kind = 'debit' and not acc.allow_negative and acc.balance - p_amount < -acc.overdraft_limit then
    raise exception 'insufficient funds on %: balance %, debit %, overdraft %',
      p_account, acc.balance, p_amount, acc.overdraft_limit using errcode = 'LE001';
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
