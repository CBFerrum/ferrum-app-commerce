# Changelog

## 0.10.2

From the first browser walk: a workspace with no Store Settings.

- **The store's reports take the store's company.** `web_sales`, `top_products`, `abandoned_carts`, `conversion_funnel` and
  `coupon_usage` named no company when the Today page asked, and a workspace with several companies and no default answered
  422 "company is required". With no company in the request the report now uses the one Store Settings name (a caller may
  still name another). With no Store Settings and no company to fall back on it answers one plain sentence: the store is not
  set up yet.
- **First-run state.** The Today page, Orders, Returns, Stock alerts, Variants, Storefront words and Money show "The store is
  not set up yet" with a link to make Store Settings (or "ask an administrator") while none exist, and ask the server nothing
  it would refuse. The store's records (catalog, coupons, Store Settings) stay reachable.
- **Plain refusals.** A refusal the server gives a screen is shown as its reason alone, not as "Please correct the highlighted
  fields: ...".
- **Routes.** The route table is in `ui/src/routes.tsx` and a test mounts it on a workspace with no Store Settings and one with
  them, and opens new and existing records. Commerce did not have CRM's route fault: every record route names the entity.

## 0.10.1

The screens for 0.10.0, rebuilt with Ferrum-Frontend. Needs nothing new of the server; `ui/dist` is rebuilt (0.10.0 left it stale).

- **Change order** asks the server what saving would do (`POST /api/app/commerce/desk/change-preview`) a moment after the last edit, and shows what it answers: the lines as priced (promotions, tax and delivery again), the totals, the difference and the `settlement` in words (the shopper is asked through which method, replacing the open request or not; sent back through which payment; owed by the shopper or the store; or nothing). The delivery method (`shipping_rule`) is offered beside it, and is asked for when the server says more than one applies. Saving sends the same body and the desk is told the server's figures, and if the price moved since the preview, that. The old estimate is gone; a refund is never approved by a preview.
- **Cancel and refund.** The refund dialog is for goods that come back; its "Cancel the order" path is removed (the server refuses it for an order paid two ways) and it points at **Cancel order…**, which sends each payment back (the gift card's part first) and shows the server's refusal as it is.
- **Order screen.** **Send a mail again…** (More menu): the four mails that may go again (Shipped, Payment received, Payment failed, Refunded), the ones with nothing to tell disabled with the reason, the count on the order and the daily limit, a note that the confirmation and gift card or store credit codes cannot, the server's refusal as it is, and how many more may go today. A **Gift card** block shows what the card paid and what it holds.
- **Storefront** (`assets/store.js`): a shopper whose card paid part and whose rest failed or lapsed is offered to pay the rest (the page asks for the rest only, as the server does) and is told what the card paid, the rest and what is left on it; the card's note shows while the gateway's page is made; numbers and country names are written in the page's language; a chosen `?lang=` stays on the store's links and its search. A test fails if a word of its own comes back into the script. Words added: `retry.text_card`.
- **Storefront Words** (`/words`, a tab for those who may change them): one row a word with its key, the English, the workspace's text, search, only the words not yet written, reset to default, a place its English does not fill refused before the server does; one record a language, as before.

Not built: the language switch (the storefront's `GET /store` does not say which languages the store offers, so the page cannot list them), and the gift card's `held_until` and `balance_left` on the desk's order screen (only the shopper's order routes carry `gift_card`; the desk sees what the card paid and holds from the order's own fields). The new words are in English in the Hindi and Arabic catalogs.

## 0.10.0

Needs ERP 0.25 or later for its tests (carriers on the platform's connections); uses the platform's links with `columns` (BD-38). What the screens of 0.9.1 asked of the server, and the storefront's words. A new entity, **Storefront Words**, so a minor release; nothing is seeded and nothing is assumed: a workspace that has not had the update has no words table, no terms field, no settings for them, and its storefront, links and mails behave as before.

- **Paying the rest after a card paid part.** `POST /orders/pay` refused with "already paid" as soon as any payment of the order was paid, a gift card's part included, so a shopper whose card covered part and whose rest failed or lapsed (as 0.8.0 left such orders: invoiced at checkout, the card redeemed) could not try again. It asks for what is left (the total less everything paid and less what a card holds), through the method chosen, as often as the shopper tries; paid in full it still says so. An order whose only payment is a card's part, and whose rest failed, expired or was cancelled, now says so (`Failed`, `Expired`, `Cancelled`) instead of `Paid`. The 0.9.0 path the notes called untested, a 0.8.0-flow order part-paid by a card and cancelled by the upkeep when its rest expires, is tested: the card has its value back, the invoice and the order are cancelled.
- **A preview of an order change.** `POST /api/site/{workspace}/commerce/desk/change-preview` (signed in, as the desk's actions) takes the body of **Update Web Order** (`sales_order`, `changes`, `address`, `shipping_rule`) and answers what saving would do, by running the save's own code in a transaction that is thrown away: `lines` as priced (promotions again, tax and delivery for where it goes), `totals` (goods, delivery, taxes, the total and what it was), `total`, `difference`, `owed`, and `settlement`: `charge` (through which method, replacing the open request or not), `refund` (to which payment's method) or `owed` (by the shopper or the store) or `none`. Update Web Order returns the same fields. **Delivery is priced again** when an edit changes the weight, the value or the place: by the method the order had (or the one that now applies; `shipping_rule` chooses when several do) for the goods it now holds, a line made when delivery was free and is not now, a refusal in words when no method reaches the place or the weight.
- **What the shopper's link pages say.** The confirmation's link shows the order's lines through the link's `columns` (item, quantity, price, amount) beside date, status and total, and the store's terms: Store Settings' new **Words on the Shopper's Page** (After Sales; `{hours}`, `{days}`, `{fee}`, `{shipping}`, `{refund}` filled from the settings, edited by the workspace; the order keeps what it said, `commerce_after_sales_terms`). The return form takes an item by its name or number (`Blue Mug x2, 3`) and says so in its hint. The link lives as long as the windows that apply: a store with no returns has one that ends with its cancellation window. The mails say what happens next and the amount: the cancellation says how much goes back and to which payments, the return request where the refund goes, the fee and who pays shipping.
- **Small server pieces.** **Send Mail Again** (order action, `mail`: Shipped, Payment Received, Payment Failed, Refunded) tells the shopper again as the order stands, to the order's own address; counted (`commerce_mails_resent`) and limited (Store Settings' **Same Mail to One Order at Most (a Day)**, 3 by default, the first included); a mail with a secret value (the confirmation's link, a gift card's or store credit's code) is not one of them. The order's and checkout's answers carry `gift_card`: `paid`, `held`, `held_until` (when a hold lets go) and `balance_left`, never the code. The refund form's **Cancellation** kind refuses an order paid two ways and points at Cancel Web Order, which sends each payment back.
- **The storefront's words.** `assets/store.js` has no word of its own: every word is a key (`t("cart.empty")`, `tn("cart.items", n)` for a count as the language counts it), read from the words the server writes into each page (`<script type="application/json" id="store-words">`, with `<html lang dir>`). `assets/words/en.json` is the English; **Storefront Words** (one record a language, a JSON object of key to text, keys and `{places}` checked) gives or overrides any language's, with English under every word it lacks. Store Settings' **Languages Offered** (the first is the default) and the visitor's choice (`?lang=`, else the browser's `Accept-Language` by preference, a code or its first part) pick the language; Arabic and the other right-to-left languages get `dir="rtl"`. No translation ships besides English. Not done: the words the server writes itself (`src/pages.rs`: the header, the listing and product pages, 404) are still English.

Screens that must follow: the order's **Change…** dialog calls the preview route instead of estimating, shows `settlement` and offers the delivery method; the refund dialog drops **Cancel the order** for the order's **Cancel Web Order**; the order shows **Send Mail Again** (a generic action dialog does) and what a gift card holds; Store Settings' new fields and Storefront Words are generic forms (a JSON field for the words). `ui/` is not rebuilt: its sources changed only in the storefront's own test, so `npm run check` needs the screens' rebuild.

## 0.9.1

The screens the server features of 0.8.0 and 0.9.0 needed. Needs nothing new of the server.

- **The storefront's cart and checkout** (`assets/store.js`): the cart asks where it is going (country, region, postal code) and shows the delivery methods for the place (free ones as Free, and plainly when none reaches it) with the total for it, so the total shown is the total charged; checkout re-works the total as the place is given. Every promotion code on the cart is listed with what it took and which does not apply, and a code can be added and removed in checkout too. A gift card or store credit code (and PIN) is taken at the payment step with what happens to a card that holds less than the total; the order page says what a card paid or holds. The details step asks whether the shopper agrees to a reminder for a cart they leave (`reminder_consent`). An order whose payment failed or ran out offers the same order to be paid again by a method of the shopper's choice; an order changed after it was paid shows what was paid, what it now comes to and a button to pay the difference, and what is on its way back because of the change.
- **The product page**: the option picker is told what the server knows of every combination: its price and stock, which are sold out and which are not offered together with what is chosen; an item out of stock (or the variant shown) has a box to leave an email to be told when it is back.
- **Order desk** (`/o/{id}`): **Change…** (quantities, lines added and taken away, a new address) shows the difference and what happens to it before saving (the open request replaced, the shopper asked for it, it sent back, or recorded as owed) and then the server's own figures; **Cancel order…** lists what goes back payment by payment, the gift card's part first, and what each does; what is owed either way and what a gift card holds show on the order.
- **Returns** (`/returns`): return requests to decide, waiting for the goods, and done, each with the shopper's reason and note, the terms they were given (where the refund goes, who pays return shipping, the restocking fee) and whether it came inside the window; approve and decline with a note, and receive the goods with the refund (less the fee) shown before it is confirmed. **Stock alerts** (`/alerts`) by item. **Variants** (`/variants/{id}`): a builder for Generate Variants (the options and values, the values not sold, every combination before it is made).

Not built: resending a shopper's mail (the server has no action for it), the balance left on a gift card and when a hold lets go (no route or field says them), a preview of an order change from the server (the desk's difference is an estimate until it is saved). The After Sales tab, shipping rules' regions, postal ranges and free over, and the website item's options and disabled options are the generic forms' own. The storefront's words are English, as before.

## 0.9.0

Needs ERP 0.24 or later (stored value holds, Update Items re-pricing promotions, delivery country and region, carriers). Commerce CM-1, CM-44, CM-49, CM-66, CM-68, CM-69, CM-76 and the three faults 0.8.0 left.

- **The three faults of 0.8.0.** (1) *A gift card that covers part of an order holds that part for it* (ERP's stored value hold) and nothing is invoiced until the rest is paid: the gateway's payment (or the courier's, on delivery) invoices the order, which redeems the hold, and the card's part is then recorded as before (a paid `Gift Card` payment request). The rest's request never lasts longer than the hold. If it expires, the upkeep lets the card go (`commerce_card_held` back to nothing) and cancels the order as any unpaid order is (when the store says so). A card that covers the whole order still pays at checkout. (2) *An order paid two ways cancels*: **Cancel Web Order** (new action; the same function serves the shopper) makes one cancellation refund for each payment, the card's first. A card alone is given its value back in the same transaction; a provider's part goes through its refund (a person approves it unless the action says so, the gateway carries it out, `Refund Failed` is recorded and retried) and the card's part waits for it, so a failed provider refund never leaves the order half cancelled; the order is cancelled when the last has gone back, and declined for both when one is declined. An unpaid order cancels at once and its held value goes with it. Orders from 0.8.0 that a card paid part of at checkout (invoiced then) are cancelled by the upkeep in the same way when the rest expires. (3) *A gift card's code is a secret value of its message*: sealed while the mail waits, blanked once handed over; without the server's secret key the codes are not made and the managers are told.
- **The desk changes an order before it is fulfilled (CM-68).** **Update Web Order**: lines (a quantity, 0 removes, an item added) go through ERP's Update Items (promotions priced again by ERP; what has shipped or been billed is refused in ERP's words), and the address is the order's own again (an Address, delivery country and region, taxes worked out for the new place). What the order then comes to is settled through its payment: an unpaid order's request is replaced; a paid one is asked for the difference through the method it was paid with (a payment link on the order page), or sent back as a new refund kind, **Adjustment** (a person approves it unless the action says so, the gateway carries it out, the books follow); where neither can be done the amount is recorded as owed (`Owed After a Change`, either way). The shopper is told (`commerce.order_changed`). Refused in words: a cancelled, closed or fully delivered order; one a gift card holds part of; a country the store does not ship to; an item the store does not sell. Not done: the delivery charge is not priced again by the new weight or total.
- **The shopper cancels or asks to return (CM-69).** The order confirmation carries a signed link (the platform's action links; the token is a secret value of the mail) offering **Cancel My Order** and **Return Something**. Store Settings' new **After Sales** tab: how many hours a shopper may cancel (24; 0 = not), whether a cancellation's refund is automatic, how many days after delivery a return may be asked (14; 0 = not), who pays return shipping, a restocking fee (%), where returns are refunded (the payment, or store credit of a stored value kind) and the carrier's words for "out for delivery". A return is a new **Return Request** (a shopper's lines by their number on the order, a reason, a note) that the desk approves or declines (with a note); **Receive Goods** then makes the refund as it is made today (return delivery note, credit note, the money out through the gateway, or onto new store credit whose code goes once, sealed, in a message). A restocking fee reduces what the credit note credits. The shopper is told at each step. Not done: photos (the platform's link pages take no files), an approval policy or workflow on the request (the desk's role decides), a signed-in account's own page.
- **Tax and shipping by where it goes (CM-44, CM-49, CM-76).** The cart carries the delivery country and region as soon as the shopper gives them (a known customer's cart takes that place's Address, found or made, so ERP does not price it for their last address), so the total shown is the total charged; the store's tax template is the fallback for what no country rule chooses. **Shipping rules are zones**: countries, **regions**, **postal ranges** (numbers as numbers, letters by what a code starts with) and **Free Over**, with the fixed, order-value and weight rates as before; a rule without them applies everywhere, as before. A recognised customer (signed in, or the cart's) is priced from their own price list (`default_price_list`, in the store's currency), on the cart and in the catalog. Not done: live carrier rates at checkout with a fallback to the zone rates (ERP's carriers rate a shipment's parcels, not a cart: the cart has no Address or delivery note to rate).
- **The shopper's mails (CM-66)** are message templates the package declares (editable, per language): `commerce.order_placed`, `order_cancelled`, `payment_received`, `payment_failed` (with the link to try again), `order_shipped` (as before), `out_for_delivery` and `order_delivered` (the upkeep reads ERP's carrier shipments of delivery notes the desk said to tell about, once each), `order_refunded`, `order_changed`, `return_requested`, `return_approved`, `return_declined`, `store_credit_issued`, `gift_card_issued`, `back_in_stock` (a new **Stock Alert**: `POST /items/{slug}/stock-alert {email}` on an item out of stock; told once) and `cart_reminder` (promotional, only for a cart whose shopper agreed: `reminder_consent` in the cart's details; the `commerce.cart_abandoned` event now carries `consent` for other apps' campaigns). A mail that cannot be sent is logged and never stops what happened. The fixed English mail in code is gone.
- **Variants generated as ERP's (CM-1).** **Generate Variants** on a website item takes `{"Size": ["S","M"], "Colour": ["Red"]}`: it makes the attributes and values in ERP's Stock Operations (which must be installed), marks the item as the template, has ERP's `make_variants` make the combinations as items of their own, and gives each a website item (a variant of this one with the options the product page's picker reads) and the template's price in the store's list, to be changed per combination; stock is per combination (each is an item). **Disabled Options** on the product: a value the store does not sell; its combinations are made unpublished and cannot be published.

For data already there: new fields and entities reach a workspace with its next schema update; no SQL migration; nothing the code needs is assumed to be seeded (the reasons "Cancelled before it shipped", "Order changed" and "Other" are made when absent, the new settings read as the defaults above, the new upkeep steps and the held card path check for their fields). Orders placed before keep working: a card part invoiced at checkout stays as it was. Screens that need a change: Store Settings (After Sales tab), the order screen (Cancel Web Order, Update Web Order, held and owed amounts), a return request screen, stock alerts, the shipping rule form (regions, postal ranges, free over), the website item form (Generate Variants, disabled options), the storefront's checkout details (the reminder consent), the order page (the pay-again of a difference) and the item page (a stock-alert box).

## 0.8.0

Needs ERP 0.21 or later. Promotions, gift cards and orders shipped in parts (Commerce CM-60 to CM-62, CM-42, CM-54, CM-55).

- **Promotions through ERP.** Carts and web orders are priced by ERP's promotions exactly as any sales order: amount or percent off the order or some lines, buy and get, bundles, free delivery, free items (added as lines), automatic or by code, several codes at once (up to ten a cart), combining as each promotion says. Carts and orders name the store's **Sales Channel** (new setting, default Web Shop) and its fulfilment warehouse, so a promotion for a channel or place applies to both alike. The store's Shipping Item is marked a **Delivery Charge** (on saving Store Settings and by the upkeep, so an existing store needs nothing): only free delivery takes it off. A code that stops opening a promotion drops off the cart as a lapsed coupon does; a promotion that changes between cart and checkout is caught by the total check and the order then agrees with its cart; ERP counts each code once when the order is submitted. **Coupons stay beside it:** `PUT /cart/coupon` takes a store coupon as before (one per cart, unlocking its pricing rule) or a promotion code. They were not mapped onto promotions: a coupon unlocks an item-level pricing rule (quantity ranges, a rate or discount per item), which ERP prices before promotions, so existing coupons keep their exact effect and limits without migrating rules. `DELETE /cart/codes/{code}` takes one off. The cart shows its codes and what each promotion took.
- **Gift cards** (Store Settings **Gift Cards**: an ERP stored value kind). At checkout `gift_card: {code, pin?}` pays what the card holds; the rest goes by the chosen method, which may be left out when the card covers everything. Stored value pays invoices, so such an order is invoiced at checkout; the card is redeemed through ERP's prove step and its code is never kept. What it took is a paid `Gift Card` payment request on the order (Paid by Gift Card on the order). An item naming a stored value kind sells a card: once its order is paid, the card is opened and its new code (and PIN) go to the shopper once in the `commerce.gift_card_issued` message; the order keeps only that they went (Gift Cards Sent). A refund goes back where the money came from, the card first: a gift card's refund is done on approval, loading the card again (the refund's Payment Entry); cancelling an order a card alone paid gives the value back.
- **Shipped in parts, from several warehouses.** Each order line is reserved in the warehouse that holds it: the fulfilment warehouse when it has enough, else the first of the store's other warehouses that does (so what the shop counted as available is where it ships from). **Ship Web Order** takes `lines` (`[{line, qty, warehouse}]`): some lines, part of a line, from another of the store's warehouses; left empty, everything left from where it was reserved. Each shipment is its own delivery note with its own carrier and tracking; the one that completes the order invoices it. The order's **Fulfilment** (Unfulfilled, Partly Fulfilled, Fulfilled) follows its deliveries and the shopper's order shows it. The shopper is told of each shipment, its lines and tracking, by the `commerce.order_shipped` message, which a workspace may edit (it replaces the fixed shipped mail, same wording).

For data already there: new fields reach a workspace with its next schema update; no SQL migration. Orders placed before keep their one warehouse and show their fulfilment from what was delivered.

## 0.7.22

Rebuilt with Ferrum-Frontend 45dad7f, the design system as tuned after the landed build was reviewed. The left menu's icons are chosen so that none repeats on more than three rows and each fits its row; their drawings load beside the first page instead of inside it, so the page starts lighter; a row that scrolls sideways fades at the edge where there is more, in every browser and with reduced motion; a printed record leaves out empty fields and keeps its totals with the table; menus and popovers sit inside the page's landmarks for assistive technology; a selection bar folds what does not fit into More on a narrow screen. Nothing else changes.

## 0.7.21

Tax follows where the goods are delivered. Checkout records the shopper's shipping address as a structured Address (country code, region, postcode) and sets it as the order's ship-to, which is where the country add-ons read the destination. Until now a returning customer's order took their saved address, and a new customer's had none, so the add-on fell back to the company's home region. The customer's Address for that place is reused when they have one that says the same (ignoring case and spacing), else made: a new customer's is their primary shipping address. The cart carries it too (`ship_to`, set at checkout, when the customer is known), so the cart's final pricing and its order see the same delivery region and their totals agree. Before checkout a cart is still priced without a region: a tax rule that decides by region cannot see one until there is an Address record to read, so the total shown can change at checkout and the shopper is asked to confirm it. Needs the contacts and addresses package (installed with CRM); without it nothing changes. The India add-on does not read the order's ship-to yet (it reads the customer's state or its own place-of-supply field); the other country add-ons do.

## 0.7.20

The interface follows the upgraded design system, rebuilt with Ferrum-Frontend 0a47157. Text uses the kit's type roles (nothing under 12px) and controls its height tokens; loading shows skeletons shaped like what is coming; empty, no matches, denied, offline and failed are told apart; status colours come from stored state ids. The interface check now runs the UI lint against an empty baseline. An expired payment is amber, not blue; a refused page is shown as refused.

## 0.7.19

Needs ERP 0.15 or later. A refund after an earlier one on the same order no longer fails: since ERP 0.15.0 a credit note carries only what is still returnable, each line naming the invoice line it gives back, and Commerce now cuts the note down by that line rather than by position. What can still come back is read from what ERP counts on each invoice and delivery line (Returned Qty), and the delivery note that restocks the goods names the line it gives back. When ERP refuses a credit note all the same, its sentence is shown on the refund (and in the desk's estimate) rather than an internal error. The money still goes back by a journal entry against the credit note, as before.

## 0.7.18

The reports it defines in code (their titles and parameters) have names in Arabic and Hindi too, with its models' and manifest's (404 each in ui/src/locales/names). Translated by the model, to be reviewed by native speakers.

## 0.7.17

Rebuilt with Ferrum-Frontend c59c9a2: the names of its models, fields and manifest in Arabic and Hindi (399 each in ui/src/locales/names, which the platform serves to every page of the workspace, so a record's labels, the launcher and the bar read in the person's language), and the sentences its pages still showed in English (split around a link or a figure, built in code from a template or word by word) whole and translated (344 words each in ui/src/locales). Translated by the model, to be reviewed by native speakers.

## 0.7.16

Rebuilt with Ferrum-Frontend 155518c: the sentences its pages built in code (counts, dates, what a toast or a screen reader says) go through t() too, as whole sentences where code chose a word, and carry built-in Arabic and Hindi (293 words each in ui/src/locales, translated by the model and to be reviewed by native speakers).

## 0.7.15

The assistant knows what its records mean in its pages (contributes.shell.assistant): their states, what counts and where figures come from, with questions it suggests asking there.

## 0.7.14

Rebuilt with Ferrum-Frontend 5b22e7b: its own words carry built-in Arabic and Hindi (276 words each, ui/src/locales, translated by the model and to be reviewed by native speakers), fetched for the page's language only. The labels it keeps in objects, its dialogs, its page titles and the words it keeps in variables go through t() as well, and the names and sizes it shows as they are keep their order right to left. Its pages are drawn once their words are in, and its entry carries only the parts of the kit it uses.

## 0.7.13

Rebuilt with Ferrum-Frontend 6537d65: an action's link, code or token shown once is kept in a dialog to copy, and on a page that reads right to left, English left untranslated keeps its own order. Its own words go through the kit's t() (176 places), so a workspace's own words for its language reach them, and Admin, Interface words lists them under it (dist/words.json).

## 0.7.12

Rebuilt with Ferrum-Frontend d5d98f2: a workspace's own words for its interface (Admin, Interface words) reach its pages as they load; an attachment field that holds a picture shows it; and a value another record already has is said so, where the form said the record had been changed by someone else. Its own screens' margins, paddings, borders, alignment and insets go by the line's start and end, so they mirror right to left as the kit's do; what it places by coordinates (boards, timelines, charts, handles) stays as it was.

## 0.7.11

Rebuilt with Ferrum-Frontend 63280d3: right to left, its pages mirror as they read (icons that point along the line, margins and alignment by the line's start and end, arrow keys along it), and the records pages' words are in Arabic and Hindi.

## 0.7.10

Rebuilt with Ferrum-Frontend a64416a: a new record's box whose default is 1 shows ticked, as it is saved. An action's dialog says every refusal, the errors on what the action made (another record's rows) among them, and an action with parameters and a confirmation asks once.

## 0.7.9

Rebuilt with Ferrum-Frontend a76295e: its pages show times on a person's own clock when they chose one on their Account page, while today and due dates stay on the workspace's day. Its pages count today on the workspace's day, not UTC's, so the day's figures are right in the evening west of Greenwich and the early morning east of it.

## 0.7.8

Its storefront says it serves pages at the root of a `site:commerce` host, so Ferrum's Domains offers the storefront, and only apps that serve a site (Ferrum PR 85). A shipped order reads "Shipped" to the shopper, not "Delivered": nothing tells the store a parcel arrived, and an order shipped and invoiced at once was called delivered as it left. The shopper is e-mailed through the server's mailer: the order with its link and how to pay, that it has shipped when the desk says to tell them (carrier, parcel and link), and money sent back when the refund says so. Before, only events went out, and nothing in a workspace sent them (both found walking an order through the storefront).

## 0.7.7

An order's story gives each step's full time on the workspace's clock. Rebuilt with Ferrum-Frontend 24c3951, so its records' pages show times on the workspace's clock too, a draft numbered on submit is headed as a draft, and a record of an app that is not installed says so.

## 0.7.6

Rebuilt with Ferrum-Frontend bb60115: a record page reads an empty number or check as the server does, so an action whose condition needs one shows, and a workspace's settings open as their one record.

## 0.7.5

A payment request's page asks for what its actions read: Mark Paid needs the amount and currency received and never asked, so from the request's page it always failed (the order page sends them); it now asks, filled with the request's own amount and currency, and takes the currency as the page names it. Mark Failed asks for its reason and reference, and Set Gateway Details for the payment link and reference.

## 0.7.4

Rebuilt with Ferrum-Frontend 00dea07: in a workflow state that names what may change, the other fields show read-only, and the fields the state needs show required.

## 0.7.3

Personal-data requests find shopping cart records by the person's phone number as well as their email (Ferrum's FERRUM-PRIVACY-002).

## 0.7.2

Rebuilt with Ferrum-Frontend daeadda: after an action runs, a record page shows what the action said it did rather than only that it is done.

## 0.7.1

Rebuilt with Ferrum-Frontend 80ff179: a field's label names its control for assistive technology, and a record whose title is a link shows that record's title, not its id.

## 0.7.0

For apps that take payments through the store (Bookings' deposits first):

- A payment request can ask for part of an invoice, a deposit, rather than all of it.
- A refund can be of part of a payment, not only of an order (kind Payment). The app that took the
  money works out how much goes back (a booking's cancellation terms), against the credit note it made;
  a person still approves it, the gateway carries it out, and the money out is posted as for an
  order's refund. `commerce.payment_refunded` goes out.
- `ferrum_app_commerce::request_invoice_payment` and `refund_payment` are what other apps call.

## 0.6.0, and Store Subscriptions 0.1.0

Subscriptions sold in the store (FERRUM-COMMERCE-002), in a connector app with Finance, Store
Subscriptions, offered where both are installed. Finance bills them; the store sells them and asks for
each renewal.

- A website item can be sold as a Finance subscription plan. Its web order pays the first period, and
  the subscription it starts is billed by Finance from the second, one subscription per plan line.
  Price the item at the plan's price.
- Each renewal invoice Finance makes gets a payment request through the order's payment method, to
  the order's email: a gateway plugin collects it as it does an order's. Payment requests can now pay
  an invoice as well as an order (Commerce 0.6.0): the payment is allocated to the invoice, and
  `commerce.invoice_paid` goes out.
- Signed-in customers see their subscriptions in the portal (`GET subscriptions`): the plans, the status
  and whether it gives what it sells, the period, and the renewals still to pay with the request that
  pays each. They pay a renewal through any of the store's methods and cancel, the subscription then
  running to the end of the period paid for.
- Cancelling the web order cancels the subscriptions it started.
- Other apps ask whether a customer has an active subscription to a plan with
  `ferrum_app_commerce::entitled`.

Not yet: an email of its own for a renewal (the payment request and the `commerce.invoice_paid` event
are there for a mail or gateway plugin to tell the customer), cards kept on file for charging renewals
without the customer, and changing plans from the portal.

## 0.5.0

- The store's admin has a web-order desk (Orders). It sorts the web orders into what waits for money,
  what is to be picked and sent, and what is sent but not invoiced, longest-waiting first, each with
  the one thing to do next: ship (carrier, tracking number and link, and whether to tell the shopper,
  with the invoice made at the same time), invoice, or mark a bank transfer or cash as paid. A pick list
  turns the orders to send into what to take off the shelves, and prints. Each order has a page of its
  own: items, every payment with its reference and expiry, shipments with tracking, invoices, refunds,
  the customer, where it goes and the story so far. A Website Manager ships and invoices web orders
  without holding the Sales roles (two actions on web orders do exactly that). An order placed before this
  version does not remember how it is paid, so the desk reads that from the order's latest payment request.
- Money that needs a person has one screen (Money): refunds waiting for a decision, and payments a gateway
  took and could not book. A manager approves a refund (the amount, the gateway and a warning that it
  sends money are on the confirmation), declines it with a reason, or says it was refunded another way.
  The tab counts what waits. A gateway still can never approve one.
- Refunds and returns of paid orders. Choose the delivered lines and how many come back, whether each goes
  back on the shelf, and why; or cancel an order that has not shipped. The server works out the amount
  (it makes the credit note and reads what it comes to), holds it to what the payment still has, and shows
  it as you choose. A person approves; the gateway the order was paid with carries it out and reports back;
  then the books follow, all or nothing: a return delivery note, a credit note, the money posted out of the
  bank and matched to what the credit note owes, the order closed when nothing is left to ship (or, for a
  cancellation, the payment reversed and the order cancelled). If the books refuse, the refund stays
  "Refunded", says why, and can be posted again. Cash and bank transfers are refunded by hand and recorded.
  Gateway plugins must learn to carry these out: see "Refunds of booked payments" in the package README.
- Orders can be followed from a link with no sign-in: the order's number and token in the address' fragment
  (no server or log ever sees it) open a page with the steps, the carrier and a link to the parcel, refunds
  on their way, and only the town it goes to. The link comes with the checkout result and the order and
  shipping events; the desk copies it too.
- Store items can have variants (a size, a colour). A variant is an item of its own, with its own price,
  stock and code, that says which item it is a variant of and what sets it apart. The item is listed once,
  "From" its cheapest variant; its page shows the chosen variant's price, stock and picture, and the other
  choices as plain links; orders, stock and reviews follow without change. Search finds each word of a
  query anywhere in an item, its variants and their choices.
- The storefront on a phone: sorting and "in stock only" in the address, an empty category, no search results
  and nothing in stock each say what to do next; the cart has quantity steppers and a placeholder while it
  loads; checkout has two named steps, a folded summary, each refused field marked beside the field, and for
  an online payment waits for the gateway's page; every view has a failure with a way on.
- New events: `commerce.order_shipped` and `commerce.order_refunded`; `commerce.order_placed` carries the
  order's tracking link. New fields on delivery notes (tracking number and link, whether to tell the
  customer) and on web orders (the method that pays them, and "Partly Refunded" and "Refunded" payment states).

## 0.4.0

- A payment that cannot be booked is a Payment Exception a manager sees, not a log line. Someone approves a refund,
  which the gateway carries out once however often it is retried, or records how it was settled.
- Gateways no longer hold their API keys or webhook secrets: the plugin host does.
- Personal data: a person's request now exports their payment requests, and says why they are kept: they are payment records.

## 0.3.2

- The interface starts faster: its first download holds the frame and the app's own pages, and
  lists, forms, reports and other apps' screens load when first opened (Ferrum
  docs/plan/apps-ui.md §12.9). The build keeps it within the size budget.

## 0.3.1

- Record cards (the bar's previews on hover, Ferrum apps-ui.md U5) stay off the web orders to deliver and the payments that did not go through, whose rows
  already show what a card would.

## 0.3.0

- The storefront on a host of its own (Ferrum docs/plan/apps-ui.md §7.3, U7): on a workspace host
  whose target is `site:commerce`, the shop's pages are at its root, rendered on the server so
  search engines and link previews read them: the home and categories with their items a page at
  a time, search, product pages (images, price with or without tax as the store shows it, stock,
  description as plain text, reviews, related items, schema.org Product data, Open Graph), a
  robots.txt and a sitemap.xml. Until the store has its settings the site says it opens soon.
- The cart (quantities, removing, discount codes), checkout (contact, delivery and billing
  addresses, delivery method, payment method, terms, the total the customer saw; one order per
  attempt with an Idempotency-Key) and the order page (what was ordered, what is due, paying an
  open payment request) are `assets/store.js` over the storefront API, which the platform names in
  `x-ferrum-site-api`. Product pages work without scripts; the cart needs them.
- Pages leave no markup from the catalog: names, descriptions and reviews are escaped, and images
  are the workspace's public files or https addresses.
- Requires the platform with `site:{app}` hosts (Ferrum, apps-ui.md U7).

## 0.2.0

- The store's admin, an interface of its own at /commerce/ (Ferrum docs/plan/apps-ui.md U5), in
  `ui/` (built with Ferrum-Frontend's packages, `dist` committed and embedded as `UI`): web sales
  today and over the last 7 and 30 days, web orders still to deliver (opened in ERP, where they
  are delivered and billed), payments that failed or expired, reviews waiting for moderation
  (published or rejected in place) and the catalog's state. Live updates keep it current. The
  store's records are lists and forms in the records frame.
- Launcher entry (Store) for Website and Sales Managers; Store item and Coupon in the Create menu;
  /open of the store's records into its forms.

## 0.1.1

- Requires the ERP app (Ferrum docs/plan/apps-ui.md §13): installing it in a workspace without ERP
  installs ERP whole, with its interface, rather than a few of its packages.
- Declares where it keeps people's personal data, so Administration → Personal data exports and
  erases it for a person (by email).

## 0.1.0

- First release as a standalone Ferrum app (previously part of the Ferrum repository).
