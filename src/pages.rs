//! The storefront's own pages on a `site:commerce` host (Ferrum docs/plan/apps-ui.md §7.3):
//! rendered on the server, so search engines and link previews read them, and usable without
//! scripts up to the cart. The cart, checkout and order pages are `assets/store.js` over the
//! storefront API, whose address the platform gives in `x-ferrum-site-api` along with the site's
//! own origin (`x-ferrum-site-origin`); neither ever comes from the caller.

use crate::catalog::{self, Listing, Sort, Variant, default_variant, option_groups};
use crate::store::Store;
use ferrum_kernel::{Ctx, Error, Kernel, Result, SiteRequest, SiteResponse, Tx};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::str::FromStr;

const CSS: &str = include_str!("../assets/store.css");
const JS: &str = include_str!("../assets/store.js");
/// Cards on a page of the catalog, a category or search results.
const PAGE: i64 = 24;
/// Items a sitemap lists at most (the protocol allows 50 000 addresses per file).
const SITEMAP_ITEMS: i64 = 10_000;

/// Whether a request is for the site's pages (`/_site/…`, which only the platform sends, on a
/// `site:commerce` host).
pub fn is_page(req: &SiteRequest) -> bool {
    req.path == "/_site" || req.path.starts_with("/_site/")
}

pub async fn handle(k: &Kernel, ctx: &Ctx, req: &SiteRequest) -> Result<SiteResponse> {
    let header = |n: &str| req.headers.get(n).and_then(Value::as_str).unwrap_or_default();
    let (origin, api) = (header("x-ferrum-site-origin"), header("x-ferrum-site-api"));
    let not_here = || Error::NotFound("storefront page".into());
    if req.method != "GET" || !(origin.starts_with("https://") || origin.starts_with("http://")) || !api.starts_with("/api/site/") {
        return Err(not_here());
    }
    let rest = req.path.trim_start_matches("/_site");
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        ["assets", "store.css"] => return Ok(asset("text/css; charset=utf-8", CSS)),
        ["assets", "store.js"] => return Ok(asset("text/javascript; charset=utf-8", JS)),
        _ => {}
    }
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let store = match Store::load(k, ctx, &mut tx).await {
        Ok(s) => s,
        Err(Error::NotFound(_)) => {
            tx.commit().await?;
            return Ok(closed());
        }
        Err(e) => return Err(e),
    };
    let site = Shop { origin, api, store: &store, cats: catalog::category_tree(&mut tx, ctx.tenant).await?, path: rest };
    let query = |n: &str| req.query.get(n).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let out = match segs.as_slice() {
        [] => listing(k, ctx, &mut tx, &site, Lens::of(None, None, &query), query("cursor")).await?,
        ["c", slug] => listing(k, ctx, &mut tx, &site, Lens::of(Some(slug), None, &query), query("cursor")).await?,
        ["search"] => match query("q") {
            Some(q) if q.chars().count() <= 100 => listing(k, ctx, &mut tx, &site, Lens::of(None, Some(q), &query), query("cursor")).await?,
            Some(_) => site.page(400, "Search", None, true, "<h1>Search</h1><p class=\"muted\">Search for at most 100 characters.</p>"),
            None => site.page(200, "Search", None, true, "<h1>Search</h1><p class=\"muted\">Type what you are looking for above.</p>"),
        },
        ["p", slug] => product(k, ctx, &mut tx, &site, slug, query("v")).await?,
        ["cart"] => site.script_page("Your cart", "cart", "Loading your cart…"),
        ["checkout"] => site.script_page("Checkout", "checkout", "Loading checkout…"),
        ["order"] => site.script_page("Your order", "order", "Loading your order…"),
        ["robots.txt"] => robots(origin),
        ["sitemap.xml"] => sitemap(&mut tx, ctx, &site).await?,
        _ => site.missing(),
    };
    tx.commit().await?;
    Ok(out)
}

/// What a page knows about the shop it belongs to.
struct Shop<'a> {
    origin: &'a str,
    api: &'a str,
    store: &'a Store,
    /// Published categories as a tree (`catalog::category_tree`).
    cats: Value,
    /// This page's path below the site's root (`/p/steel-bracket`).
    path: &'a str,
}

impl Shop<'_> {
    /// The page around `main`: the store's name, search, cart, its top categories.
    fn page(&self, status: u16, title: &str, meta: Option<&Meta>, noindex: bool, main: &str) -> SiteResponse {
        let store = esc(&self.store.name);
        let cats: String = self
            .cats
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| Some(format!(r#"<a href="/c/{}">{}</a>"#, esc(c["slug"].as_str()?), esc(c["name"].as_str().unwrap_or_default()))))
            .collect();
        let nav = if cats.is_empty() { String::new() } else { format!(r#"<nav class="cats" aria-label="Categories">{cats}</nav>"#) };
        let description = meta.and_then(|m| m.description.as_deref()).map(|d| format!(r#"<meta name="description" content="{}">"#, esc(d))).unwrap_or_default();
        let canonical = format!("{}{}", self.origin, if self.path.is_empty() { "/" } else { self.path });
        let social = meta.map(|m| {
            let image = m.image.as_deref().map(|i| format!(r#"<meta property="og:image" content="{}">"#, esc(&absolute(self.origin, i)))).unwrap_or_default();
            format!(
                r#"<meta property="og:type" content="{}"><meta property="og:title" content="{}"><meta property="og:url" content="{}">{image}"#,
                m.kind,
                esc(title),
                esc(&canonical)
            )
        });
        let robots = if noindex { r#"<meta name="robots" content="noindex">"# } else { "" };
        let data =
            meta.and_then(|m| m.data.as_ref()).map(|d| format!(r#"<script type="application/ld+json">{}</script>"#, json_in_html(d))).unwrap_or_default();
        let return_to = self
            .store
            .store_url
            .as_deref()
            .filter(|u| u.trim_end_matches('/') == self.origin)
            .map(|_| format!(r#" data-return="{}/order""#, esc(self.origin)))
            .unwrap_or_default();
        let terms = self
            .store
            .terms_url
            .as_deref()
            .filter(|u| u.starts_with("https://") || u.starts_with('/'))
            .map(|u| format!(r#" · <a href="{}">Terms</a>"#, esc(u)))
            .unwrap_or_default();
        let html = format!(
            r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{t}{sep}{store}</title>{description}<link rel="canonical" href="{c}">{social}{robots}<link rel="stylesheet" href="/assets/store.css?v={v}"><script type="module" src="/assets/store.js?v={v}"></script>{data}</head><body data-api="{api}" data-currency="{cur}" data-precision="{prec}" data-tax="{tax}"{return_to}><a class="skip" href="#main">Skip to content</a><header class="top"><a class="brand" href="/">{store}</a><form class="search" action="/search" role="search"><input name="q" type="search" placeholder="Search the store" aria-label="Search the store" maxlength="100"></form><a class="cart-link" href="/cart">Cart<span class="count" data-cart-count hidden></span></a></header>{nav}<main id="main">{main}</main><div id="live" class="visually-hidden" role="status" aria-live="polite"></div><footer><span>© {store}</span>{terms}</footer></body></html>"##,
            t = esc(title),
            sep = if title.is_empty() { "" } else { " · " },
            c = esc(&canonical),
            social = social.unwrap_or_default(),
            v = version(),
            api = esc(self.api),
            cur = esc(&self.store.currency_code),
            prec = self.store.precision,
            tax = self.store.display_prices_with_tax,
        );
        page_response(status, html)
    }

    /// A page the script fills (cart, checkout, order), never indexed.
    fn script_page(&self, title: &str, view: &str, loading: &str) -> SiteResponse {
        let main = format!(
            r#"<h1>{t}</h1><div id="view" data-view="{view}" aria-busy="true"><div class="skeleton" aria-hidden="true"><span></span><span></span></div><p class="visually-hidden">{l}</p></div><noscript><p class="notice">This page needs JavaScript. Your cart, checkout and order work once it is on.</p></noscript>"#,
            t = esc(title),
            l = esc(loading)
        );
        self.page(200, title, None, true, &main)
    }

    fn missing(&self) -> SiteResponse {
        self.page(
            404,
            "Page not found",
            None,
            true,
            r#"<h1>Page not found</h1><p class="muted">There is nothing at this address. <a href="/">Go to the store</a>.</p>"#,
        )
    }

    /// A price as shown: with tax when the store shows it so, the list price struck through when lower.
    fn price(&self, p: &Value) -> String {
        let code = p["currency"].as_str().unwrap_or(&self.store.currency_code);
        let with_tax = p["price_with_tax"].as_str().filter(|_| self.store.display_prices_with_tax);
        let shown = with_tax.or(p["price"].as_str()).unwrap_or("0");
        let discount = p["discount_percent"].as_str().and_then(|d| Decimal::from_str(d).ok()).is_some_and(|d| d > Decimal::ZERO);
        let was = if discount && with_tax.is_none() {
            p["list_price"].as_str().map(|l| format!(r#" <s class="was">{}</s>"#, money(code, l, self.store.precision))).unwrap_or_default()
        } else {
            String::new()
        };
        format!(r#"<span class="price">{}</span>{was}"#, money(code, shown, self.store.precision))
    }

    fn cards(&self, items: &[Value]) -> String {
        if items.is_empty() {
            return String::new();
        }
        let li: String = items
            .iter()
            .filter_map(|c| {
                let slug = c["slug"].as_str()?;
                let img = image(&c["image"], 400);
                let stock = if c["in_stock"].as_bool() == Some(false) { r#"<span class="out">Out of stock</span>"# } else { "" };
                let price = c.get("price").filter(|p| !p.is_null()).map(|p| self.price(p)).unwrap_or_default();
                let price = if c["from"].as_bool() == Some(true) && !price.is_empty() { format!(r#"<span class="from">From</span> {price}"#) } else { price };
                Some(format!(
                    r#"<li><a class="card" href="/p/{}">{img}<span class="name">{}</span><span class="cost">{price}</span>{stock}</a></li>"#,
                    esc(slug),
                    esc(c["title"].as_str().unwrap_or(slug))
                ))
            })
            .collect();
        format!(r#"<ul class="cards">{li}</ul>"#)
    }
}

/// What a page tells search engines and link previews about itself.
struct Meta {
    kind: &'static str,
    description: Option<String>,
    image: Option<String>,
    data: Option<Value>,
}

/// What a listing is of, and how the shopper asked to see it (all from the address): a category or a
/// search, sorted, and perhaps only what is in stock.
struct Lens<'a> {
    category: Option<&'a str>,
    q: Option<&'a str>,
    sort: Sort,
    in_stock: bool,
}

impl<'a> Lens<'a> {
    fn of(category: Option<&'a str>, q: Option<&'a str>, query: &dyn Fn(&str) -> Option<&'a str>) -> Lens<'a> {
        Lens { category, q, sort: Sort::parse(query("sort")).unwrap_or(Sort::Ranking), in_stock: matches!(query("in_stock"), Some("1" | "true")) }
    }

    /// The address's own query for the next page or the same page again: what is asked, then `more`.
    fn query(&self, more: &str) -> String {
        let mut qs = String::new();
        if let Some(q) = self.q {
            qs.push_str(&format!("q={}&", enc(q)));
        }
        if self.sort != Sort::Ranking {
            qs.push_str(&format!("sort={}&", self.sort.name()));
        }
        if self.in_stock {
            qs.push_str("in_stock=1&");
        }
        qs.push_str(more);
        qs.trim_end_matches('&').to_string()
    }
}

/// The sort and stock choices over a listing: a plain form (works without scripts; the script
/// submits it when a choice changes).
fn refine_form(site: &Shop<'_>, lens: &Lens<'_>) -> String {
    let options: String = [
        (Sort::Ranking, "Featured"),
        (Sort::PriceAsc, "Price: low to high"),
        (Sort::PriceDesc, "Price: high to low"),
        (Sort::Newest, "Newest"),
        (Sort::Name, "Name"),
        (Sort::Rating, "Best rated"),
    ]
    .iter()
    .map(|(s, label)| format!(r#"<option value="{}"{}>{label}</option>"#, s.name(), if *s == lens.sort { " selected" } else { "" }))
    .collect();
    let hidden = lens.q.map(|q| format!(r#"<input type="hidden" name="q" value="{}">"#, esc(q))).unwrap_or_default();
    format!(
        r#"<form class="refine" action="{}" method="get" aria-label="Sort and filter">{hidden}<label>Sort by <select name="sort">{options}</select></label><label><input type="checkbox" name="in_stock" value="1"{}> In stock only</label><button class="quiet" type="submit">Apply</button></form>"#,
        esc(if site.path.is_empty() { "/" } else { site.path }),
        if lens.in_stock { " checked" } else { "" }
    )
}

/// What a listing says when it has no cards, in words that tell the shopper what to do next.
fn nothing(lens: &Lens<'_>, category: bool) -> String {
    let (title, hint) = match (lens.q, lens.in_stock) {
        (Some(q), _) => {
            (format!("No results for “{}”", esc(q)), r#"Check the spelling, or try fewer or different words. <a href="/">Browse the store</a>."#.to_string())
        }
        (None, true) => (
            "Nothing in stock here".to_string(),
            format!(r#"Everything here is sold out for now. <a href="{}">Show it all</a>."#, esc(&format!("?{}", Lens { in_stock: false, ..*lens }.query("")))),
        ),
        (None, false) if category => ("No items in this category yet".to_string(), r#"<a href="/">Browse the rest of the store</a>."#.to_string()),
        (None, false) => ("This store has no items yet".to_string(), "Come back soon.".to_string()),
    };
    format!(r#"<div class="nothing"><h2>{title}</h2><p class="muted">{hint}</p></div>"#)
}

/// The home page, a category or search results: cards, a page at a time.
async fn listing(k: &Kernel, ctx: &Ctx, tx: &mut Tx, site: &Shop<'_>, lens: Lens<'_>, cursor: Option<&str>) -> Result<SiteResponse> {
    let cat = match lens.category {
        Some(slug) => match find_category(&site.cats, slug) {
            Some(c) => Some(c.clone()),
            None => return Ok(site.missing()),
        },
        None => None,
    };
    let q = lens.q;
    let f =
        Listing { category: lens.category.map(str::to_string), q: q.map(str::to_string), min_price: None, max_price: None, in_stock: lens.in_stock, ids: None };
    let found = catalog::cards(k, ctx, tx, site.store, &f, lens.sort, PAGE, cursor, None).await;
    let (items, next) = match found {
        Ok(f) => f,
        // A cursor from another listing, or one tampered with: start again.
        Err(Error::Invalid(_) | Error::Validation(_)) if cursor.is_some() => return Ok(redirect(site.path)),
        Err(Error::NotFound(_)) => return Ok(site.missing()),
        Err(e) => return Err(e),
    };
    let more = next
        .map(|n| {
            format!(
                r#"<p class="more"><a class="button" href="{}?{}">More</a></p>"#,
                esc(if site.path.is_empty() { "/" } else { site.path }),
                esc(&lens.query(&format!("cursor={}", enc(&n))))
            )
        })
        .unwrap_or_default();
    let refine = if items.is_empty() && !lens.in_stock && lens.q.is_none() { String::new() } else { refine_form(site, &lens) };
    let grid = if items.is_empty() { nothing(&lens, cat.is_some()) } else { format!("{}{more}", site.cards(&items)) };
    Ok(match (&cat, q) {
        (Some(c), _) => {
            let name = c["name"].as_str().unwrap_or_default();
            let about = c["description"].as_str().filter(|d| !d.trim().is_empty()).map(|d| format!(r#"<p class="lead">{}</p>"#, esc(d))).unwrap_or_default();
            let subs: String = c["children"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| Some(format!(r#"<a class="chip" href="/c/{}">{}</a>"#, esc(s["slug"].as_str()?), esc(s["name"].as_str().unwrap_or_default()))))
                .collect();
            let subs = if subs.is_empty() { subs } else { format!(r#"<p class="chips">{subs}</p>"#) };
            let title = c["seo"]["title"].as_str().filter(|t| !t.is_empty()).unwrap_or(name);
            let meta = Meta {
                kind: "website",
                description: c["seo"]["description"].as_str().or(c["description"].as_str()).map(str::to_string),
                image: c["image"].as_str().map(str::to_string),
                data: None,
            };
            let crumbs = r#"<p class="crumbs"><a href="/">Home</a></p>"#;
            site.page(
                200,
                title,
                Some(&meta),
                cursor.is_some() || lens.in_stock || lens.sort != Sort::Ranking,
                &format!(r#"{crumbs}<h1>{}</h1>{about}{subs}{refine}{grid}"#, esc(name)),
            )
        }
        (None, Some(q)) => site.page(200, &format!("Search: {q}"), None, true, &format!(r#"<h1>Results for “{}”</h1>{refine}{grid}"#, esc(q))),
        (None, None) => {
            let meta = Meta { kind: "website", description: None, image: None, data: None };
            site.page(
                200,
                "",
                Some(&meta),
                cursor.is_some() || lens.in_stock || lens.sort != Sort::Ranking,
                &format!(r#"<h1 class="visually-hidden">{}</h1>{refine}{grid}"#, esc(&site.store.name)),
            )
        }
    })
}

async fn product(k: &Kernel, ctx: &Ctx, tx: &mut Tx, site: &Shop<'_>, slug: &str, chosen: Option<&str>) -> Result<SiteResponse> {
    let p = match catalog::item_page(k, ctx, tx, site.store, slug, None).await {
        Ok(p) => p,
        Err(Error::NotFound(_)) => return Ok(site.missing()),
        Err(e) => return Err(e),
    };
    // A variant is shown on its item's page, with its choice selected.
    if let Some(parent) = p["variant_of"].as_object() {
        return Ok(match (parent.get("slug").and_then(Value::as_str), parent.get("published").and_then(Value::as_bool)) {
            (Some(parent), Some(true)) => moved(&format!("/p/{}?v={}", enc(parent), enc(slug))),
            _ => site.missing(),
        });
    }
    let title = p["title"].as_str().unwrap_or(slug);
    // An item with variants is sold through them: the page shows the one chosen (or the first that can be bought).
    let variants: Vec<Variant> = p["variants"].as_array().into_iter().flatten().filter_map(Variant::from_json).collect();
    let selected: Option<&Value> = if variants.is_empty() {
        None
    } else {
        let want = chosen.filter(|c| variants.iter().any(|v| v.slug == *c)).map(str::to_string).or_else(|| default_variant(&variants).map(|v| v.slug.clone()));
        want.and_then(|w| p["variants"].as_array().and_then(|a| a.iter().find(|v| v["slug"].as_str() == Some(w.as_str()))))
    };
    let shown = selected.unwrap_or(&p);
    let buy_slug = shown["slug"].as_str().filter(|_| selected.is_some()).unwrap_or(slug);
    let mut images: Vec<&Value> = p["images"].as_array().into_iter().flatten().filter(|i| safe_image(i["url"].as_str()).is_some()).collect();
    if let Some(own) = selected.map(|v| &v["image"]).filter(|i| safe_image(i["url"].as_str()).is_some()) {
        images.retain(|i| i["url"] != own["url"]);
        images.insert(0, own);
    }
    let gallery = match images.split_first() {
        Some((first, rest)) => {
            let thumbs: String = rest.iter().map(|i| image(i, 160)).collect();
            format!(
                r#"<div class="gallery">{}{}</div>"#,
                image(first, 800),
                if thumbs.is_empty() { thumbs } else { format!(r#"<div class="thumbs">{thumbs}</div>"#) }
            )
        }
        None => r#"<div class="gallery"><span class="img none" aria-hidden="true"></span></div>"#.into(),
    };
    let crumbs = p["categories"]
        .as_array()
        .and_then(|c| c.first())
        .and_then(|c| {
            Some(format!(
                r#"<p class="crumbs"><a href="/">Home</a> / <a href="/c/{}">{}</a></p>"#,
                esc(c["slug"].as_str()?),
                esc(c["name"].as_str().unwrap_or_default())
            ))
        })
        .unwrap_or_default();
    let price = shown.get("price").filter(|v| !v.is_null()).map(|v| site.price(v)).unwrap_or_default();
    let in_stock = shown["in_stock"].as_bool().unwrap_or(true);
    let stock = match (in_stock, shown["available_qty"].as_str().or(shown["available_qty"].as_i64().map(|_| "")).filter(|q| !q.is_empty())) {
        (false, _) => r#"<p class="out">Out of stock</p>"#.to_string(),
        (true, Some(q)) if site.store.show_stock_quantity => format!(r#"<p class="in">{} in stock</p>"#, esc(q)),
        (true, _) => r#"<p class="in">In stock</p>"#.to_string(),
    };
    // What sets the shown variant apart, and the choices that lead to the others (plain links: they work without scripts).
    let picks: String = match selected.and_then(|s| s["slug"].as_str()) {
        Some(on) => option_groups(&variants, on)
            .into_iter()
            .map(|(name, choices)| {
                let now = choices.iter().find(|c| c.selected).map(|c| c.value.as_str()).unwrap_or_default();
                let links: String = choices
                    .iter()
                    .map(|c| {
                        format!(
                            r#"<a class="pick{}{}" href="{}?v={}"{} aria-label="{}{}">{}</a>"#,
                            if c.selected { " on" } else { "" },
                            if c.in_stock { "" } else { " off" },
                            esc(site.path),
                            esc(&enc(&c.slug)),
                            if c.selected { r#" aria-current="true""# } else { "" },
                            esc(&c.value),
                            if c.in_stock { "" } else { ", sold out" },
                            esc(&c.value),
                        )
                    })
                    .collect();
                format!(r#"<fieldset class="option"><legend>{}: <strong>{}</strong></legend><div class="picks">{links}</div></fieldset>"#, esc(&name), esc(now))
            })
            .collect(),
        None => String::new(),
    };
    let label = selected
        .map(|s| s["options"].as_array().into_iter().flatten().filter_map(|o| o["value"].as_str()).collect::<Vec<_>>().join(" / "))
        .filter(|l| !l.is_empty())
        .map(|l| format!(r#"<p class="variant-name">{}</p>"#, esc(&l)))
        .unwrap_or_default();
    let whole = p["unit"]["whole_number"].as_bool().unwrap_or(true);
    let max = p["max_qty"].as_str().unwrap_or("1000");
    let for_sale = if selected.is_some() { shown["purchasable"].as_bool() == Some(true) } else { p["purchasable"].as_bool() == Some(true) };
    let buy = if for_sale && (in_stock || !site.store.enforce_stock) {
        format!(
            r#"<form class="add" action="/cart" method="get" data-item="{s}"><input type="hidden" name="add" value="{s}"><label>Quantity <input name="qty" type="number" min="{min}" max="{max}" step="{step}" value="1" inputmode="{mode}"></label><button class="button" type="submit">Add to cart</button><p class="added" role="status"></p></form>"#,
            s = esc(buy_slug),
            min = if whole { "1" } else { "0.001" },
            max = esc(max),
            step = if whole { "1" } else { "any" },
            mode = if whole { "numeric" } else { "decimal" },
        )
    } else if selected.is_some() && !for_sale {
        r#"<p class="muted">This choice is not for sale.</p>"#.to_string()
    } else {
        String::new()
    };
    let short = p["short_description"].as_str().map(|s| format!(r#"<p class="lead">{}</p>"#, esc(s))).unwrap_or_default();
    let description = p["description"]
        .as_str()
        .map(paragraphs)
        .filter(|d| !d.is_empty())
        .map(|d| format!(r#"<section class="about"><h2>About this item</h2>{d}</section>"#))
        .unwrap_or_default();
    let rating = &p["rating"];
    let count = rating["count"].as_i64().unwrap_or(0);
    let reviews: String = p["reviews"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let stars = r["rating"].as_i64().unwrap_or(0).clamp(0, 5) as usize;
            format!(
                r#"<li><p><span class="stars" aria-label="{stars} of 5">{}{}</span> <strong>{}</strong></p>{}<p class="muted">{}{}{}</p></li>"#,
                "★".repeat(stars),
                "☆".repeat(5 - stars),
                esc(r["title"].as_str().unwrap_or_default()),
                r["review"].as_str().map(|t| format!("<p>{}</p>", esc(t))).unwrap_or_default(),
                esc(r["reviewer_name"].as_str().unwrap_or("A customer")),
                r["date"].as_str().map(|d| format!(" · {}", esc(d))).unwrap_or_default(),
                if r["verified_purchase"].as_bool() == Some(true) { " · Verified purchase" } else { "" },
            )
        })
        .collect();
    let reviews = if count > 0 {
        format!(
            r#"<section class="reviews"><h2>Reviews</h2><p>{} out of 5 from {count} {}</p><ul>{reviews}</ul></section>"#,
            esc(rating["average"].as_str().unwrap_or("0")),
            if count == 1 { "review" } else { "reviews" }
        )
    } else {
        String::new()
    };
    let related = p["related"]
        .as_array()
        .filter(|r| !r.is_empty())
        .map(|r| format!(r#"<section class="related"><h2>You may also like</h2>{}</section>"#, site.cards(r)))
        .unwrap_or_default();
    let first_image = images.first().and_then(|i| i["url"].as_str()).map(str::to_string);
    let offered = shown["price"]["price_with_tax"].as_str().filter(|_| site.store.display_prices_with_tax).or(shown["price"]["price"].as_str());
    let mut data = json!({
        "@context": "https://schema.org",
        "@type": "Product",
        "name": title,
        "sku": p["sku"],
        "description": p["seo"]["description"],
        "image": first_image.as_deref().map(|i| absolute(site.origin, i)),
    });
    if let Some(price) = offered {
        data["offers"] = json!({"@type": "Offer", "price": price, "priceCurrency": site.store.currency_code,
            "availability": if in_stock { "https://schema.org/InStock" } else { "https://schema.org/OutOfStock" }, "url": format!("{}/p/{slug}", site.origin)});
    }
    if count > 0 {
        data["aggregateRating"] = json!({"@type": "AggregateRating", "ratingValue": rating["average"], "reviewCount": count});
    }
    let meta = Meta { kind: "product", description: p["seo"]["description"].as_str().map(str::to_string), image: first_image, data: Some(data) };
    let seo_title = p["seo"]["title"].as_str().unwrap_or(title);
    let main = format!(
        r#"{crumbs}<article class="product">{gallery}<div class="buy"><h1>{}</h1>{label}<p class="cost">{price}</p>{stock}{picks}{short}{buy}</div></article>{description}{reviews}{related}"#,
        esc(title)
    );
    Ok(site.page(200, seo_title, Some(&meta), false, &main))
}

fn robots(origin: &str) -> SiteResponse {
    let body = format!("User-agent: *\nDisallow: /cart\nDisallow: /checkout\nDisallow: /order\nDisallow: /search\nAllow: /\n\nSitemap: {origin}/sitemap.xml\n");
    let mut r = SiteResponse::text(200, "text/plain; charset=utf-8", body);
    r.headers.push(("cache-control".into(), "public, max-age=3600".into()));
    r
}

async fn sitemap(tx: &mut Tx, ctx: &Ctx, site: &Shop<'_>) -> Result<SiteResponse> {
    let items: Vec<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "select w.slug, w.updated_at from e_website_item w join e_item i on i.tenant_id = w.tenant_id and i.id = w.item \
         where w.tenant_id = $1 and w.published and w.variant_of is null and not coalesce(i.disabled, false) and coalesce(w.slug, '') <> '' order by w.slug limit $2",
    )
    .bind(ctx.tenant)
    .bind(SITEMAP_ITEMS)
    .fetch_all(&mut **tx)
    .await?;
    let mut urls = vec![format!("<url><loc>{}/</loc></url>", esc(site.origin))];
    let mut stack: Vec<&Value> = site.cats.as_array().into_iter().flatten().collect();
    while let Some(c) = stack.pop() {
        if let Some(slug) = c["slug"].as_str() {
            urls.push(format!("<url><loc>{}/c/{}</loc></url>", esc(site.origin), esc(&enc(slug))));
        }
        stack.extend(c["children"].as_array().into_iter().flatten());
    }
    for (slug, at) in &items {
        urls.push(format!("<url><loc>{}/p/{}</loc><lastmod>{}</lastmod></url>", esc(site.origin), esc(&enc(slug)), at.format("%Y-%m-%d")));
    }
    let body = format!(r#"<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{}</urlset>"#, urls.concat());
    let mut r = SiteResponse::text(200, "application/xml; charset=utf-8", body);
    r.headers.push(("cache-control".into(), "public, max-age=3600".into()));
    Ok(r)
}

/// Until the store's settings exist, the storefront is closed.
fn closed() -> SiteResponse {
    let html = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="robots" content="noindex"><title>Opening soon</title><link rel="stylesheet" href="/assets/store.css"></head><body><main id="main"><h1>Opening soon</h1><p class="muted">This store is not open yet. Come back in a while.</p></main></body></html>"#;
    let mut r = page_response(503, html.into());
    r.headers.push(("retry-after".into(), "3600".into()));
    r
}

fn find_category<'a>(cats: &'a Value, slug: &str) -> Option<&'a Value> {
    cats.as_array()?.iter().find_map(|c| if c["slug"].as_str() == Some(slug) { Some(c) } else { find_category(&c["children"], slug) })
}

fn page_response(status: u16, html: String) -> SiteResponse {
    let mut r = SiteResponse::text(status, "text/html; charset=utf-8", html);
    // Prices and stock change; a page is good for a minute in shared caches, no longer.
    r.headers.push(("cache-control".into(), "public, max-age=60".into()));
    r
}

/// A page that lives elsewhere for good (a variant's address becomes its item's).
fn moved(to: &str) -> SiteResponse {
    let mut r = SiteResponse::text(301, "text/plain; charset=utf-8", "");
    r.headers.push(("location".into(), to.to_string()));
    r
}

fn redirect(to: &str) -> SiteResponse {
    let mut r = SiteResponse::text(303, "text/plain; charset=utf-8", "");
    r.headers.push(("location".into(), if to.is_empty() { "/".into() } else { to.to_string() }));
    r
}

fn asset(content_type: &str, body: &str) -> SiteResponse {
    let mut r = SiteResponse::text(200, content_type, body);
    // Pages name assets with their version, so an asset at one address never changes.
    r.headers.push(("cache-control".into(), "public, max-age=31536000, immutable".into()));
    r
}

/// The assets' version, for their addresses.
fn version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| hex::encode(&Sha256::digest(format!("{CSS}{JS}").as_bytes())[..6]))
}

/// An image the page may show: public files of the workspace, or https addresses.
fn safe_image(url: Option<&str>) -> Option<&str> {
    url.filter(|u| u.starts_with("/files/public/") || (u.starts_with("https://") && !u.contains(['"', '<', '>', ' '])))
}

fn image(v: &Value, size: u32) -> String {
    match safe_image(v["url"].as_str()) {
        Some(url) => format!(
            r#"<img src="{}" alt="{}" width="{size}" height="{size}" loading="lazy" decoding="async">"#,
            esc(url),
            esc(v["alt"].as_str().unwrap_or_default())
        ),
        None => r#"<span class="img none" aria-hidden="true"></span>"#.into(),
    }
}

fn absolute(origin: &str, url: &str) -> String {
    if url.starts_with('/') { format!("{origin}{url}") } else { url.to_string() }
}

/// Text written as Markdown, shown as plain paragraphs: nothing in it becomes markup.
fn paragraphs(text: &str) -> String {
    text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).map(|p| format!("<p>{}</p>", esc(p).replace('\n', "<br>"))).collect()
}

/// An amount with its currency, rounded to the store's precision, thousands grouped.
pub fn money(code: &str, amount: &str, precision: u32) -> String {
    let Ok(d) = Decimal::from_str(amount.trim()) else { return esc(&format!("{code} {amount}")) };
    let s = format!("{:.*}", precision as usize, d.round_dp(precision).abs());
    let (int, frac) = s.split_once('.').map_or((s.as_str(), None), |(i, f)| (i, Some(f)));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let sign = if d.is_sign_negative() && !d.is_zero() { "-" } else { "" };
    esc(&format!("{code} {sign}{grouped}{}", frac.map(|f| format!(".{f}")).unwrap_or_default()))
}

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A query string value or path segment, percent-encoded.
pub(crate) fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// JSON inside a `<script>` data block: `<` escaped, so it cannot close the element.
fn json_in_html(v: &Value) -> String {
    v.to_string().replace('<', "\\u003c")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_read_as_people_read_them() {
        assert_eq!(money("INR", "1250", 2), "INR 1,250.00");
        assert_eq!(money("INR", "1234567.891", 2), "INR 1,234,567.89");
        assert_eq!(money("JPY", "999", 0), "JPY 999");
        assert_eq!(money("USD", "-12.5", 2), "USD -12.50");
        assert_eq!(money("USD", "0", 2), "USD 0.00");
        assert_eq!(money("USD", "x<y", 2), "USD x&lt;y");
    }

    #[test]
    fn nothing_from_the_catalog_becomes_markup() {
        assert_eq!(paragraphs("Strong <b>steel</b>\nzinc\n\n\nSecond"), "<p>Strong &lt;b&gt;steel&lt;/b&gt;<br>zinc</p><p>Second</p>");
        assert_eq!(image(&json!({"url": "javascript:alert(1)"}), 10), r#"<span class="img none" aria-hidden="true"></span>"#);
        assert_eq!(image(&json!({"url": "https://x.test/a\" onerror=\"x"}), 10), r#"<span class="img none" aria-hidden="true"></span>"#);
        assert!(image(&json!({"url": "/files/public/7", "alt": "A \"bracket\""}), 10).contains(r#"alt="A &quot;bracket&quot;""#));
        assert_eq!(json_in_html(&json!({"name": "</script><script>x"})), r#"{"name":"\u003c/script>\u003cscript>x"}"#);
        assert_eq!(enc("a b/ü"), "a%20b%2F%C3%BC");
        let cats = json!([{"slug": "tools", "children": [{"slug": "drills", "children": []}]}]);
        assert_eq!(find_category(&cats, "drills").unwrap()["slug"], "drills");
        assert!(find_category(&cats, "nails").is_none());
    }
}
