-- Ledger schema v1. Applied identically to every node (one node = one cell's primary).
-- Runs with search_path set to the ledger schema by the migrator.
--
-- Rules this schema enforces itself, so no caller can break them:
--   * every table carries tenant_id, and keys include it, so tenants never mix;
--   * entries are append-only: they cannot be updated, and can only be deleted when a fence
--     has voided them (the move into quarantine);
--   * no column can hold personal data. Every text column is an opaque identifier with a
--     fixed format, or a value from a short fixed list. There are no free-text, JSON or
--     binary columns, so a name, address, card number or payment reference has nowhere to go;
--   * entries are unique per (account, epoch, seq) and per (journal, account, leg).
--
-- Entry sign convention (kept from the Python harness): amount is signed by its effect on the
-- account balance, so a credit is positive and a debit is negative. ledger-core's Posting uses
-- the opposite sign (positive is a debit); the store converts at its boundary.

-- Opaque identifiers, prefixed by kind, as in docs/api-design.md#conventions.
create domain tenant_ref   as text check (value ~ '^ten_[a-z0-9]{1,64}$');
create domain account_ref  as text check (value ~ '^acc_[a-z0-9]{1,64}$');
create domain journal_ref  as text check (value ~ '^jr_[a-z0-9]{1,64}$');
create domain entry_ref    as text check (value ~ '^ent_[a-z0-9]{1,64}$');
create domain hold_ref     as text check (value ~ '^hld_[a-z0-9]{1,64}$');
create domain command_ref  as text check (value ~ '^cmd_[a-z0-9]{1,64}$');
create domain party_ref    as text check (value ~ '^pty_[a-z0-9]{1,64}$');
create domain decision_ref as text check (value ~ '^dec_[a-z0-9]{1,64}$');
create domain actor_ref    as text check (value ~ '^(agt|usr|sys)_[a-z0-9]{1,64}$');
-- A cell or node, such as aws-lon-03.
create domain node_name    as text check (value ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$');
create domain currency_code as text check (value ~ '^[A-Z]{3}$');
-- A caller's retry key. Characters are restricted so it cannot carry a sentence.
create domain idempotency_key as text check (value ~ '^[A-Za-z0-9_-]{1,64}$');
-- A command type such as transfer.internal.
create domain journal_type as text
  check (length(value) <= 48 and value ~ '^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)?$');

-- Accounts. `balance` is a projection of entries, computed locally on every node and never
-- replicated as state. `next_seq` only means something at the account's home. The home cell
-- itself is held by the control plane, not here.
create table account (
  tenant_id       tenant_ref  not null,
  id              account_ref not null,
  kind            text        not null check (kind in ('customer', 'internal', 'general_ledger')),
  currency        currency_code not null,
  normal_side     text        not null check (normal_side in ('debit', 'credit')),
  party_id        party_ref,
  balance         bigint      not null default 0,
  next_seq        bigint      not null default 1 check (next_seq >= 1),
  allow_negative  boolean     not null default false,
  overdraft_limit bigint      not null default 0 check (overdraft_limit >= 0),
  created_at      timestamptz not null default now(),
  primary key (tenant_id, id)
);

-- One row per epoch, pushed from the control plane to every node. Entries of prev_epoch with
-- seq above prev_cutoff_seq are void, and so is any epoch strictly between prev_epoch and
-- epoch. This is what fences a stale home.
create table epoch_fence (
  tenant_id       tenant_ref  not null,
  account_id      account_ref not null,
  epoch           bigint      not null check (epoch >= 1),
  home            node_name   not null,
  prev_epoch      bigint      not null default 0 check (prev_epoch >= 0),
  prev_cutoff_seq bigint      not null default 0 check (prev_cutoff_seq >= 0),
  primary key (tenant_id, account_id, epoch),
  foreign key (tenant_id, account_id) references account (tenant_id, id),
  check (prev_epoch < epoch)
);

-- A journal: one business event, recorded at the cell that created it. Entries refer to it by
-- id only, with no foreign key, because replicated entries can arrive before it does.
create table journal (
  tenant_id       tenant_ref      not null,
  id              journal_ref     not null,
  idempotency_key idempotency_key not null,
  type            journal_type    not null,
  state           text            not null default 'pending' check (state in ('pending', 'posted', 'voided')),
  effective_date  date            not null,
  actor           actor_ref       not null,
  command_id      command_ref,
  decision_id     decision_ref,
  created_at      timestamptz     not null default now(),
  primary key (tenant_id, id),
  unique (tenant_id, idempotency_key)
);

-- Append-only log. See the sign convention above.
create table entry (
  tenant_id    tenant_ref  not null,
  id           entry_ref   not null,
  journal_id   journal_ref not null,
  account_id   account_ref not null,
  epoch        bigint      not null check (epoch >= 1),
  seq          bigint      not null check (seq >= 1),
  leg          smallint    not null default 1 check (leg >= 1),
  kind         text        not null check (kind in ('debit', 'credit')),
  amount       bigint      not null,
  counterparty account_ref,
  origin       node_name   not null,
  value_date   date,
  created_at   timestamptz not null default clock_timestamp(),
  primary key (tenant_id, id),
  unique (tenant_id, account_id, epoch, seq),
  unique (tenant_id, journal_id, account_id, leg),
  foreign key (tenant_id, account_id) references account (tenant_id, id),
  check ((kind = 'debit' and amount < 0) or (kind = 'credit' and amount > 0))
);

-- Entries never change. They leave the log in one case only: a fence voided them, and they
-- move to quarantine. The same test as the void rule in ledger/node.py.
create function entry_is_void(e entry) returns boolean language sql stable as $$
  select exists (
    select 1 from epoch_fence f
    where f.tenant_id = e.tenant_id and f.account_id = e.account_id
      and ((f.prev_epoch = e.epoch and e.seq > f.prev_cutoff_seq)
        or (f.prev_epoch < e.epoch and e.epoch < f.epoch)))
$$;

create function entry_guard() returns trigger language plpgsql as $$
begin
  if tg_op = 'UPDATE' then
    raise exception 'entries are append-only and cannot be updated' using errcode = '23000';
  end if;
  if not entry_is_void(old) then
    raise exception 'entries are append-only: only a void entry can leave the log' using errcode = '23000';
  end if;
  return old;
end $$;

create trigger entry_append_only before update or delete on entry
  for each row execute function entry_guard();

-- Entries written by a stale home, or received from one. Never merged silently.
create table quarantine (
  tenant_id      tenant_ref  not null,
  id             entry_ref   not null,
  journal_id     journal_ref not null,
  account_id     account_ref not null,
  epoch          bigint      not null,
  seq            bigint      not null,
  leg            smallint    not null default 1,
  kind           text        not null check (kind in ('debit', 'credit')),
  amount         bigint      not null,
  counterparty   account_ref,
  origin         node_name   not null,
  value_date     date,
  created_at     timestamptz not null,
  reason         text        not null check (reason in ('void_on_arrival', 'void_after_fence')),
  quarantined_at timestamptz not null default now(),
  resolved       boolean     not null default false,
  primary key (tenant_id, id)
);
create index quarantine_unresolved on quarantine (tenant_id) where not resolved;

-- Local marker: every leg of this journal is applied. Written at the first leg's home.
create table settlement (
  tenant_id  tenant_ref  not null,
  journal_id journal_ref not null,
  settled_at timestamptz not null default now(),
  primary key (tenant_id, journal_id)
);

-- A hold reserves an amount against an account without moving its posted balance.
create table hold (
  tenant_id  tenant_ref  not null,
  id         hold_ref    not null,
  account_id account_ref not null,
  amount     bigint      not null check (amount > 0),
  state      text        not null default 'active' check (state in ('active', 'released', 'converted', 'expired')),
  expires_at timestamptz,
  command_id command_ref,
  journal_id journal_ref,
  created_at timestamptz not null default now(),
  primary key (tenant_id, id),
  unique (tenant_id, command_id),
  foreign key (tenant_id, account_id) references account (tenant_id, id),
  -- A hold that became a journal says which; no other hold does.
  check ((state = 'converted') = (journal_id is not null))
);
create index hold_active on hold (tenant_id, account_id) where state = 'active';

-- The in-transit internal account for each ordered pair of cells. A journal across cells
-- posts through it, so each cell's books balance at every instant.
create table in_transit_account (
  tenant_id  tenant_ref  not null,
  from_cell  node_name   not null,
  to_cell    node_name   not null,
  account_id account_ref not null,
  created_at timestamptz not null default now(),
  primary key (tenant_id, from_cell, to_cell),
  unique (tenant_id, account_id),
  foreign key (tenant_id, account_id) references account (tenant_id, id),
  check (from_cell <> to_cell)
);

-- The outbox: one row per entry this cell wrote, in the same transaction as the entry.
-- Other cells pull rows after the position they last saw. Position order agrees with
-- (epoch, seq) order within an account because the home writes under the account lock.
create table outbox (
  tenant_id  tenant_ref  not null,
  position   bigint      generated always as identity,
  entry_id   entry_ref   not null,
  account_id account_ref not null,
  epoch      bigint      not null,
  seq        bigint      not null,
  created_at timestamptz not null default now(),
  primary key (tenant_id, position),
  unique (tenant_id, entry_id)
);

-- How far this cell has read each other cell's outbox.
create table outbox_cursor (
  tenant_id     tenant_ref not null,
  source_cell   node_name  not null,
  last_position bigint     not null default 0 check (last_position >= 0),
  updated_at    timestamptz not null default now(),
  primary key (tenant_id, source_cell)
);

-- Each home publishes the position it has reached, even when idle, so replicas can report
-- honest lag and the strong tier can confirm an acknowledgement.
create table cell_watermark (
  tenant_id    tenant_ref not null,
  cell         node_name  not null,
  position     bigint     not null check (position >= 0),
  published_at timestamptz not null default now(),
  primary key (tenant_id, cell)
);
