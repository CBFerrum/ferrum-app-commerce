-- Commerce: the per-tenant key that signs guest order access tokens, kept out of every entity
-- (so no API, export or audit trail can reveal it), and an index for stock reservations.

create table commerce_secret (
  tenant_id uuid primary key references tenant(id) on delete cascade,
  secret bytea not null check (length(secret) = 32),
  created_at timestamptz not null default now()
);
alter table commerce_secret enable row level security;
alter table commerce_secret force row level security;
create policy tenant_isolation on commerce_secret
  using (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()))
  with check (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()));

-- Open sales orders reserve their undelivered quantities; storefront availability reads them on
-- every catalog page, so find them without scanning every order.
create index if not exists commerce_open_sales_orders on e_sales_order (tenant_id)
  where docstatus = 1 and status in ('To Deliver and Bill', 'To Deliver');
