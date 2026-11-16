-- Open sales orders reserve their undelivered quantities; storefront availability reads them on
-- every catalog page, so find them without scanning every order.
create index if not exists commerce_open_sales_orders on e_sales_order (tenant_id)
  where docstatus = 1 and status in ('To Deliver and Bill', 'To Deliver');
