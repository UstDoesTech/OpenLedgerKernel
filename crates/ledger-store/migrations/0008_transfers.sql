-- The write path for transfers over HTTP (K-12).

-- Gets this cell ready to carry value between two cells: registers both in-transit accounts and
-- fences the one homed here at epoch 1, so legs can post to it. Safe to call again, and on every
-- cell that takes part. `p_here` is the cell the call runs at, and must be one of the two.
create function prepare_in_transit(
  p_tenant text, p_from_cell text, p_to_cell text, p_currency text,
  p_sending_account text, p_receiving_account text, p_here text
) returns void language plpgsql
  set search_path from current
as $$
begin
  if p_here <> p_from_cell and p_here <> p_to_cell then
    raise exception 'cell % is neither % nor %', p_here, p_from_cell, p_to_cell using errcode = '22023';
  end if;
  perform register_in_transit(p_tenant, p_from_cell, p_to_cell, p_currency, p_sending_account, p_receiving_account);
  insert into epoch_fence (tenant_id, account_id, epoch, home)
  values (p_tenant::tenant_ref,
          (case when p_here = p_from_cell then p_sending_account else p_receiving_account end)::account_ref,
          1, p_here::node_name)
  on conflict do nothing;
end $$;

-- A transfer between two accounts homed at this cell: the journal, both legs and the settlement,
-- in one call, so a cell never holds half of it. Safe to repeat: it returns what was written the
-- first time. A decline writes nothing, because the whole call fails.
create function post_same_cell(
  p_tenant text, p_journal text, p_actor text,
  p_payer text, p_payee text,
  p_amount bigint, p_currency text, p_origin text,
  p_payer_epoch bigint, p_payee_epoch bigint,
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
  select * into credit from post_leg(p_tenant, p_journal, p_payee, 2::smallint, 'credit', p_amount, p_currency,
                                     p_payer, p_origin, p_payee_epoch, p_entry_credit);
  update journal j set state = 'posted'
   where j.tenant_id = p_tenant::tenant_ref and j.id = p_journal::journal_ref and j.state = 'pending';
  insert into settlement (tenant_id, journal_id) values (p_tenant::tenant_ref, p_journal::journal_ref)
  on conflict do nothing;
  return query select credit.watermark, debit.balance, debit.replayed and credit.replayed;
end $$;
