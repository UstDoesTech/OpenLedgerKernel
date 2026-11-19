-- What the API service needs from the database (K-11). Two things, and nothing that can hold
-- personal data or free text:
--
--   * api_request: one row per idempotency key, so a repeat of a request with the same body is
--     recognised and the same key with a different body is refused. It holds the hash of the
--     request and the id of the thing the request made, never the request itself. A repeat
--     rebuilds its answer from the account or the journal, so the answer is the original.
--   * open_account: makes an account and its first epoch at its home cell, in one call.

create domain request_hash as text check (value ~ '^[0-9a-f]{64}$');
create domain resource_ref as text check (value ~ '^(acc|jr)_[a-z0-9]{1,64}$');

create table api_request (
  tenant_id       tenant_ref      not null,
  principal       actor_ref       not null,
  idempotency_key idempotency_key not null,
  kind            text            not null check (kind in ('account.open', 'transfer')),
  request_hash    request_hash    not null,
  resource_id     resource_ref    not null,
  created_at      timestamptz     not null default now(),
  primary key (tenant_id, principal, idempotency_key)
);

-- Opens an account at its home, at the epoch the control plane gave it. Safe to repeat: it
-- returns false when the account was already there and writes nothing.
create function open_account(
  p_tenant text, p_account text, p_currency text, p_overdraft_limit bigint,
  p_home text, p_epoch bigint
) returns boolean
  language plpgsql
  set search_path from current
as $$
declare
  made boolean;
begin
  insert into account (tenant_id, id, kind, currency, normal_side, allow_negative, overdraft_limit)
  values (p_tenant::tenant_ref, p_account::account_ref, 'customer', p_currency::currency_code,
          'credit', false, p_overdraft_limit)
  on conflict (tenant_id, id) do nothing;
  made := found;
  insert into epoch_fence (tenant_id, account_id, epoch, home)
  values (p_tenant::tenant_ref, p_account::account_ref, p_epoch, p_home::node_name)
  on conflict (tenant_id, account_id, epoch) do nothing;
  return made;
end $$;
