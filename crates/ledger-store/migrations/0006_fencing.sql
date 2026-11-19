-- Fencing and quarantine on every node (R-02). The void rule of ledger/node.py is the function
-- entry_is_void (migration 1): an entry is void if its epoch was superseded after it, with a
-- sequence beyond the cutoff the next home started from, or if its epoch was skipped entirely.
-- Until now only the apply path in migration 5 enforced it. These make the database enforce it
-- however an entry or a fence gets here, so no writer can get around it:
--
--   * installing a fence sweeps the account at once: entries it voids move to quarantine and
--     leave the balance, whether this cell wrote them or received them;
--   * an entry that is void, or that is already in quarantine, cannot be inserted into the log;
--   * quarantined entries stay in quarantine. Nothing here merges one back.
--
-- SQLSTATEs: LE003 a fence for an epoch that already exists with different details,
-- LE011 the entry is void, LE012 the entry is in quarantine. LE004 is an unknown account.

-- Installing a fence, by any route, sweeps the account under its row lock.
create function epoch_fence_sweep() returns trigger
  language plpgsql
  set search_path from current
as $$
begin
  perform 1 from account a where a.tenant_id = new.tenant_id and a.id = new.account_id for update;
  perform repl_sweep(new.tenant_id::text, new.account_id::text);
  return null;
end $$;

create trigger epoch_fence_install after insert on epoch_fence
  for each row execute function epoch_fence_sweep();

-- Nothing void, and nothing already quarantined, enters the log.
create function entry_admit() returns trigger
  language plpgsql
  set search_path from current
as $$
begin
  if entry_is_void(new) then
    raise exception 'entry % is void: epoch % of % was superseded', new.id, new.epoch, new.account_id
      using errcode = 'LE011';
  end if;
  if exists (select 1 from quarantine q where q.tenant_id = new.tenant_id and q.id = new.id) then
    raise exception 'entry % is in quarantine and is never merged', new.id using errcode = 'LE012';
  end if;
  return new;
end $$;

create trigger entry_admission before insert on entry
  for each row execute function entry_admit();

-- Installs a fence the control plane issued. Safe to repeat: the same fence changes nothing, and
-- the same epoch with different details is refused. Returns true when it installed the fence.
create function install_fence(
  p_tenant text, p_account text, p_epoch bigint, p_home text, p_prev_epoch bigint, p_prev_cutoff bigint
) returns boolean
  language plpgsql
  set search_path from current
as $$
declare
  f epoch_fence%rowtype;
begin
  perform 1 from account a
   where a.tenant_id = p_tenant::tenant_ref and a.id = p_account::account_ref for update;
  if not found then
    raise exception 'unknown account %', p_account using errcode = 'LE004';
  end if;
  select * into f from epoch_fence x
   where x.tenant_id = p_tenant::tenant_ref and x.account_id = p_account::account_ref and x.epoch = p_epoch;
  if found then
    if f.home <> p_home::node_name or f.prev_epoch <> p_prev_epoch or f.prev_cutoff_seq <> p_prev_cutoff then
      raise exception 'epoch % of % already has a different fence', p_epoch, p_account using errcode = 'LE003';
    end if;
    return false;
  end if;
  insert into epoch_fence (tenant_id, account_id, epoch, home, prev_epoch, prev_cutoff_seq)
  values (p_tenant::tenant_ref, p_account::account_ref, p_epoch, p_home::node_name, p_prev_epoch, p_prev_cutoff);
  return true;
end $$;

-- The highest (epoch, seq) this cell holds for an account: what a standby reports when it is
-- promoted, which becomes the cutoff for the old epoch. (0, 0) when it holds nothing.
create function high_water(p_tenant text, p_account text) returns table (epoch bigint, seq bigint)
  language sql stable
  set search_path from current
as $$
  select coalesce((select e.epoch from entry e
                    where e.tenant_id = p_tenant::tenant_ref and e.account_id = p_account::account_ref
                    order by e.epoch desc, e.seq desc limit 1), 0)::bigint,
         coalesce((select e.seq from entry e
                    where e.tenant_id = p_tenant::tenant_ref and e.account_id = p_account::account_ref
                    order by e.epoch desc, e.seq desc limit 1), 0)::bigint
$$;
