-- Journals across cells (docs/design.md, "Journals across cells"). A transfer whose payer and
-- payee live on different cells is posted in legs, and every cell's books balance at every
-- instant because each cell writes a balanced pair:
--
--   at the payer's cell   debit the payer, credit the sending in-transit account     (legs 1, 2)
--   at the payee's cell   debit the receiving in-transit account, credit the payee   (legs 3, 4)
--   at the payer's cell   mark the journal posted
--
-- Each account has exactly one writer, its home, so replication can carry every entry without a
-- clash. That is why a cell pair has two in-transit accounts: the sending account is homed at
-- the payer's cell (`in_transit_account`, from migration 1) and the receiving account is homed at
-- the payee's cell (`in_transit_receipt`, below). The value still between legs is the sending
-- balance plus the receiving balance. It is zero when the pair has drained.
--
-- Each pair of legs is one SQL call, so a cell never holds half of a pair. A journal declined at
-- the first leg writes nothing, because the whole call fails.

-- The receiving account for each ordered pair of cells. Homed at to_cell. It goes negative by
-- the value that has been received, so it must be allowed to.
create table in_transit_receipt (
  tenant_id  tenant_ref  not null,
  from_cell  node_name   not null,
  to_cell    node_name   not null,
  account_id account_ref not null,
  created_at timestamptz not null default now(),
  primary key (tenant_id, from_cell, to_cell),
  unique (tenant_id, account_id),
  foreign key (tenant_id, from_cell, to_cell) references in_transit_account (tenant_id, from_cell, to_cell),
  foreign key (tenant_id, account_id) references account (tenant_id, id)
);

-- Makes both in-transit accounts for an ordered pair of cells, in one currency. Safe to call
-- again with the same arguments. Run it on every node, so each knows both accounts.
create function register_in_transit(
  p_tenant text, p_from_cell text, p_to_cell text, p_currency text,
  p_sending_account text, p_receiving_account text
) returns void language plpgsql
  set search_path from current
as $$
begin
  if p_from_cell = p_to_cell then
    raise exception 'a pair of cells needs two different cells' using errcode = '22023';
  end if;
  insert into account (tenant_id, id, kind, currency, normal_side, allow_negative)
  values (p_tenant::tenant_ref, p_sending_account::account_ref, 'internal', p_currency::currency_code, 'credit', true),
         (p_tenant::tenant_ref, p_receiving_account::account_ref, 'internal', p_currency::currency_code, 'debit', true)
  on conflict (tenant_id, id) do nothing;
  insert into in_transit_account (tenant_id, from_cell, to_cell, account_id)
  values (p_tenant::tenant_ref, p_from_cell::node_name, p_to_cell::node_name, p_sending_account::account_ref)
  on conflict (tenant_id, from_cell, to_cell) do nothing;
  insert into in_transit_receipt (tenant_id, from_cell, to_cell, account_id)
  values (p_tenant::tenant_ref, p_from_cell::node_name, p_to_cell::node_name, p_receiving_account::account_ref)
  on conflict (tenant_id, from_cell, to_cell) do nothing;
  if (select t.account_id from in_transit_account t
       where t.tenant_id = p_tenant::tenant_ref and t.from_cell = p_from_cell::node_name and t.to_cell = p_to_cell::node_name)
     <> p_sending_account::account_ref
     or (select r.account_id from in_transit_receipt r
          where r.tenant_id = p_tenant::tenant_ref and r.from_cell = p_from_cell::node_name and r.to_cell = p_to_cell::node_name)
     <> p_receiving_account::account_ref then
    raise exception 'the cells % and % already have different in-transit accounts', p_from_cell, p_to_cell
      using errcode = 'LE007';
  end if;
end $$;

-- Step 1, at the payer's cell: the journal is recorded as pending, the payer is debited (after
-- the funds check) and the sending in-transit account is credited, in one call. A repeat of the
-- same journal returns what was written the first time.
create function post_send(
  p_tenant text, p_journal text, p_actor text,
  p_payer text, p_payee text, p_sending text,
  p_amount bigint, p_currency text, p_origin text,
  p_payer_epoch bigint, p_sending_epoch bigint,
  p_entry_debit text, p_entry_credit text
) returns table (watermark bigint, balance bigint, replayed boolean)
  language plpgsql
  set search_path from current
as $$
declare
  debit  record;
  credit record;
begin
  insert into journal (tenant_id, id, idempotency_key, type, effective_date, actor)
  values (p_tenant::tenant_ref, p_journal::journal_ref, substr(p_journal, 4), 'transfer.internal', current_date, p_actor::actor_ref)
  on conflict (tenant_id, id) do nothing;
  select * into debit from post_leg(p_tenant, p_journal, p_payer, 1::smallint, 'debit', p_amount, p_currency,
                                    p_payee, p_origin, p_payer_epoch, p_entry_debit);
  select * into credit from post_leg(p_tenant, p_journal, p_sending, 2::smallint, 'credit', p_amount, p_currency,
                                     p_payee, p_origin, p_sending_epoch, p_entry_credit);
  return query select credit.watermark, debit.balance, debit.replayed and credit.replayed;
end $$;

-- Step 2, at the payee's cell: the receiving in-transit account is debited and the payee is
-- credited, in one call. A repeat returns what was written the first time, so recovery can
-- always try again.
create function post_receive(
  p_tenant text, p_journal text,
  p_payer text, p_payee text, p_receiving text,
  p_amount bigint, p_currency text, p_origin text,
  p_receiving_epoch bigint, p_payee_epoch bigint,
  p_entry_debit text, p_entry_credit text
) returns table (watermark bigint, balance bigint, replayed boolean)
  language plpgsql
  set search_path from current
as $$
declare
  debit  record;
  credit record;
begin
  select * into debit from post_leg(p_tenant, p_journal, p_receiving, 3::smallint, 'debit', p_amount, p_currency,
                                    p_payee, p_origin, p_receiving_epoch, p_entry_debit);
  select * into credit from post_leg(p_tenant, p_journal, p_payee, 4::smallint, 'credit', p_amount, p_currency,
                                     p_payer, p_origin, p_payee_epoch, p_entry_credit);
  return query select credit.watermark, credit.balance, debit.replayed and credit.replayed;
end $$;

-- Step 3, at the payer's cell: both halves are applied, so mark the journal posted. Returns true
-- when this call did it and false when it was already done, so one recovery in many finishes a
-- journal exactly once.
create function settle_journal(p_tenant text, p_journal text) returns boolean
  language plpgsql
  set search_path from current
as $$
declare
  changed boolean;
begin
  update journal j set state = 'posted'
   where j.tenant_id = p_tenant::tenant_ref and j.id = p_journal::journal_ref and j.state = 'pending';
  changed := found;
  insert into settlement (tenant_id, journal_id) values (p_tenant::tenant_ref, p_journal::journal_ref)
  on conflict do nothing;
  return changed;
end $$;
