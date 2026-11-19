-- Replication apply (R-01). A destination cell pulls another cell's outbox and applies the
-- entries here, in batches, in one SQL call per batch. Everything that decides is in the
-- database, so the relay cannot get around it. The rules are those of ledger/node.py:
--
--   * an entry already here is a duplicate and changes nothing (idempotent by entry id);
--   * an entry the fences void goes to quarantine and never merges;
--   * an entry whose epoch this cell does not know yet is deferred, never guessed at;
--   * an entry that is not the next in (epoch, seq) for its account is a gap. Gaps and
--     deferrals are buffered in repl_buffer and tried again whenever the batch ends, and the
--     relay fetches what is missing. They are never skipped.
--
-- Failures carry SQLSTATE LE003 (a different entry already holds that position, or the entry is
-- behind the local epoch) and LE004 (unknown account). Both stop the batch and roll it back.

-- Entries that arrived before their turn. Same columns as an entry, no constraint beyond the kind.
create table repl_buffer (
  tenant_id    tenant_ref  not null,
  id           entry_ref   not null,
  journal_id   journal_ref not null,
  account_id   account_ref not null,
  epoch        bigint      not null,
  seq          bigint      not null,
  leg          smallint    not null,
  kind         text        not null check (kind in ('debit', 'credit')),
  amount       bigint      not null,
  counterparty account_ref,
  origin       node_name   not null,
  created_at   timestamptz not null,
  source_cell  node_name,
  buffered_at  timestamptz not null default now(),
  primary key (tenant_id, id)
);
create index repl_buffer_order on repl_buffer (tenant_id, account_id, epoch, seq);

-- Moves the account's void entries into quarantine and takes them out of the balance. The one
-- way entries leave the log. The caller holds the account row lock.
create function repl_sweep(p_tenant text, p_account text) returns void
  language plpgsql
  set search_path from current
as $$
begin
  with void as (
    delete from entry e
     where e.tenant_id = p_tenant::tenant_ref and e.account_id = p_account::account_ref
       and entry_is_void(e)
    returning e.*
  ), q as (
    insert into quarantine (tenant_id, id, journal_id, account_id, epoch, seq, leg, kind, amount,
                            counterparty, origin, value_date, created_at, reason)
    select v.tenant_id, v.id, v.journal_id, v.account_id, v.epoch, v.seq, v.leg, v.kind, v.amount,
           v.counterparty, v.origin, v.value_date, v.created_at, 'void_after_fence' from void v
    on conflict (tenant_id, id) do nothing
  )
  update account a set balance = a.balance - coalesce((select sum(v.amount) from void v), 0)
   where a.tenant_id = p_tenant::tenant_ref and a.id = p_account::account_ref;
end $$;

-- Applies one entry. Returns applied, dup, quarantined, gap or deferred.
create function repl_apply_one(
  p_tenant text, p_id text, p_journal text, p_account text, p_epoch bigint, p_seq bigint,
  p_leg smallint, p_kind text, p_amount bigint, p_counterparty text, p_origin text,
  p_created timestamptz
) returns text language plpgsql
  set search_path from current
as $$
declare
  t        tenant_ref  := p_tenant::tenant_ref;
  a        account_ref := p_account::account_ref;
  he       bigint;
  hs       bigint;
  cutoff   bigint;
  latest   bigint;
  is_void  boolean;
begin
  perform 1 from account x where x.tenant_id = t and x.id = a for update;
  if not found then
    raise exception 'unknown account % on this cell', p_account using errcode = 'LE004';
  end if;
  if exists (select 1 from entry e where e.tenant_id = t and e.id = p_id::entry_ref) then
    return 'dup';
  end if;
  perform repl_sweep(p_tenant, p_account);

  select exists (select 1 from epoch_fence f
                  where f.tenant_id = t and f.account_id = a
                    and ((f.prev_epoch = p_epoch and p_seq > f.prev_cutoff_seq)
                      or (f.prev_epoch < p_epoch and p_epoch < f.epoch)))
    into is_void;
  if is_void then
    insert into quarantine (tenant_id, id, journal_id, account_id, epoch, seq, leg, kind, amount,
                            counterparty, origin, created_at, reason)
    values (t, p_id::entry_ref, p_journal::journal_ref, a, p_epoch, p_seq, p_leg, p_kind, p_amount,
            p_counterparty::account_ref, p_origin::node_name, p_created, 'void_on_arrival')
    on conflict (tenant_id, id) do nothing;
    return 'quarantined';
  end if;

  select coalesce(max(f.epoch), 0) into latest from epoch_fence f
   where f.tenant_id = t and f.account_id = a;
  if p_epoch > latest then
    return 'deferred';
  end if;

  select e.epoch, e.seq into he, hs from entry e
   where e.tenant_id = t and e.account_id = a order by e.epoch desc, e.seq desc limit 1;
  if not found then
    he := 0;
    hs := 0;
  end if;
  if p_epoch = he then
    if p_seq > hs + 1 then
      return 'gap';
    end if;
    if p_seq <= hs then
      raise exception 'position (%, %, %) is taken by a different entry', p_account, he, p_seq
        using errcode = 'LE003';
    end if;
  elsif p_epoch > he then
    if p_seq <> 1 then
      return 'gap';
    end if;
    select min(f.prev_cutoff_seq) into cutoff from epoch_fence f
     where f.tenant_id = t and f.account_id = a and f.prev_epoch = he;
    if cutoff is null then
      return 'deferred';
    end if;
    if hs < cutoff then
      return 'gap';
    end if;
  else
    raise exception 'entry from epoch % is behind the local epoch % of %', p_epoch, he, p_account
      using errcode = 'LE003';
  end if;

  insert into entry (tenant_id, id, journal_id, account_id, epoch, seq, leg, kind, amount,
                     counterparty, origin, created_at)
  values (t, p_id::entry_ref, p_journal::journal_ref, a, p_epoch, p_seq, p_leg, p_kind, p_amount,
          p_counterparty::account_ref, p_origin::node_name, p_created);
  update account x set balance = x.balance + p_amount where x.tenant_id = t and x.id = a;
  return 'applied';
end $$;

-- Tries every buffered entry again, in (account, epoch, seq) order, until none moves. Returns
-- the ones that left the buffer, as drained_applied, drained_dup or drained_quarantined.
create function repl_drain(p_tenant text) returns table (entry_id text, outcome text)
  language plpgsql
  set search_path from current
as $$
declare
  progress boolean := true;
  r        record;
  o        text;
begin
  while progress loop
    progress := false;
    for r in select * from repl_buffer b where b.tenant_id = p_tenant::tenant_ref
              order by b.account_id, b.epoch, b.seq
    loop
      o := repl_apply_one(r.tenant_id::text, r.id::text, r.journal_id::text, r.account_id::text,
                          r.epoch, r.seq, r.leg, r.kind, r.amount, r.counterparty::text,
                          r.origin::text, r.created_at);
      if o in ('applied', 'dup', 'quarantined') then
        delete from repl_buffer b where b.tenant_id = r.tenant_id and b.id = r.id;
        entry_id := r.id::text;
        outcome := 'drained_' || o;
        return next;
        progress := true;
      end if;
    end loop;
  end loop;
end $$;

-- Applies a batch, then tries the buffer again, then moves the cursor for the source cell
-- forward, all in one call. The arrays line up by index and are applied in the order given.
-- p_source and p_last_position may be null, for entries fetched to fill a gap rather than
-- pulled from the outbox. The cursor never moves back.
create function apply_entries(
  p_tenant text, p_source text, p_last_position bigint,
  p_ids text[], p_journals text[], p_accounts text[], p_epochs bigint[], p_seqs bigint[],
  p_legs smallint[], p_kinds text[], p_amounts bigint[], p_counterparties text[],
  p_origins text[], p_created text[]
) returns table (entry_id text, outcome text)
  language plpgsql
  set search_path from current
as $$
declare
  i int;
  o text;
begin
  for i in 1 .. coalesce(array_length(p_ids, 1), 0) loop
    o := repl_apply_one(p_tenant, p_ids[i], p_journals[i], p_accounts[i], p_epochs[i], p_seqs[i],
                        p_legs[i], p_kinds[i], p_amounts[i], p_counterparties[i], p_origins[i],
                        p_created[i]::timestamptz);
    if o in ('gap', 'deferred') then
      insert into repl_buffer (tenant_id, id, journal_id, account_id, epoch, seq, leg, kind, amount,
                               counterparty, origin, created_at, source_cell)
      values (p_tenant::tenant_ref, p_ids[i]::entry_ref, p_journals[i]::journal_ref,
              p_accounts[i]::account_ref, p_epochs[i], p_seqs[i], p_legs[i], p_kinds[i], p_amounts[i],
              p_counterparties[i]::account_ref, p_origins[i]::node_name, p_created[i]::timestamptz,
              p_source::node_name)
      on conflict (tenant_id, id) do nothing;
    end if;
    entry_id := p_ids[i];
    outcome := o;
    return next;
  end loop;
  return query select d.entry_id, d.outcome from repl_drain(p_tenant) d;
  if p_source is not null and p_last_position is not null then
    insert into outbox_cursor (tenant_id, source_cell, last_position)
    values (p_tenant::tenant_ref, p_source::node_name, p_last_position)
    on conflict (tenant_id, source_cell) do update
      set last_position = greatest(outbox_cursor.last_position, excluded.last_position),
          updated_at = now();
  end if;
end $$;

-- Publishes how far this cell's outbox has reached, even when nothing was written, so replicas
-- can report honest lag. Call it on a timer. Returns the published position.
create function publish_watermark(p_tenant text, p_cell text) returns bigint
  language plpgsql
  set search_path from current
as $$
declare
  result bigint;
begin
  insert into cell_watermark (tenant_id, cell, position, published_at)
  values (p_tenant::tenant_ref, p_cell::node_name,
          (select coalesce(max(o.position), 0) from outbox o where o.tenant_id = p_tenant::tenant_ref),
          now())
  on conflict (tenant_id, cell) do update
    set position = greatest(cell_watermark.position, excluded.position), published_at = now()
  returning cell_watermark.position into result;
  return result;
end $$;
