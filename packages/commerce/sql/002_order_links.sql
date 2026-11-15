-- Commerce 0.11: an order's access token is signed with a key derived from Commerce's own key in the vault
-- (src/keys.rs). The workspace's key in commerce_secret signs nothing any more: when it stopped. It still checks the
-- tokens of orders made before (from the vault, where it moves sealed), never those of an order made from 0.11 on,
-- which are listed here. The build before this one still reads commerce_secret; its rows go in the release after this one.
alter table commerce_secret add column if not exists retired_at timestamptz;

create table if not exists commerce_order_signed (
  tenant_id uuid not null references tenant(id) on delete cascade,
  sales_order uuid not null,
  primary key (tenant_id, sales_order)
);
alter table commerce_order_signed enable row level security;
alter table commerce_order_signed force row level security;
create policy tenant_isolation on commerce_order_signed
  using (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()))
  with check (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()));
