# API surface — reference client → lucina_* proxy routes

Upstream paths inventoried from the Retrofit annotations of the decompiled
reference client (see `~/Projects/bring-apk-analysis`, decompiled 2026-10-21).
URL paths are functional facts; the proxy (`crates/lucina-proxy/src/routes.rs`)
maps them to `lucina_<action>` routes. Two upstream bases:

- **legacy**: `api.getbring.com/rest` — auth, lists, items, user settings
- **services**: `production.bringapi.app` — per-family `/rest` prefixes
  (user, offers, discounts, prediction, push, partner, ad)

## Dedicated routes

| lucina route | method+upstream | purpose |
|---|---|---|
| `/lucina_auth` | POST `v2/bringauth` | email/password login |
| `/lucina_auth_token` | POST `v2/bringauth/token` | token refresh |
| `/lucina_signup` | POST `v2/bringauth/signup` | account signup |
| `/lucina_signup_anon` | POST `v2/bringauth/signupanonym` | anonymous account |
| `/lucina_magic_login` | POST `v2/bringauth/magiclogin` | magic-link login |
| `/lucina_lists/{user}` | GET `bringusers/{uuid}/lists` | all lists of a user |
| `/lucina_list/{list}` | GET `v2/bringlists/{uuid}` | full list snapshot |
| `/lucina_list_users/{list}` | GET `v2/bringlists/{uuid}/users` | list members |
| `/lucina_list_details/{list}` | GET `bringlists/{uuid}/details` | legacy details |
| `/lucina_list_create/{list}` | POST `bringlists/{uuid}` | create list |
| `/lucina_items_put/{list}` | PUT `v2/bringlists/{uuid}/items` | **batch item write (sync flush)** |
| `/lucina_item_details/{item}` | GET `v2/bringlistitemdetails/{uuid}` | item details |
| `/lucina_item_details_post` | POST `v2/bringlistitemdetails` | item details write |
| `/lucina_item_patch/{item}/{field}` | PUT `v2/bringlistitemdetails/{uuid}/{usersection\|usericon\|image\|assignedto}` | item patches |
| `/lucina_user/{user}` | GET `v2/bringusers/{uuid}` | profile |
| `/lucina_user_put/{user}` | PUT `v2/bringusers/{uuid}` | profile update |
| `/lucina_settings/{user}` | GET `bringusersettings/{uuid}` | settings read |
| `/lucina_settings_set/{user}/{key}` | POST `bringusersettings/{uuid}/{key}` | settings write |
| `/lucina_providers` | GET (user) `v1/providers` | retail catalog |
| `/lucina_provider_stores/{p}` | GET (user) `v1/providers/{id}/stores` | stores of a provider |
| `/lucina_offers` | GET (offers) `v1/offers` | offers |
| `/lucina_discounts_check` | POST (discounts) `v1/discounts/check` | discount check |
| `/lucina_predict` | POST (prediction) `v1/itempredictor/prediction` | item prediction |
| `/lucina_device_register` | POST (push) `bringdevices` | push device registration |
| `/lucina_catalog_local` | — (local file) | offline catalog asset (`LUCINA_CATALOG`) |

## Generic passthrough

`/lucina_any/{family}/{*path}` reaches every other inventoried upstream
operation without a dedicated route (e.g. templates, invitations, statistics,
wallet, year-in-review, sponsored products). Family is `legacy` or one of the
service families above.

## Full inventoried upstream surface (reference)

Auth: `v2/bringauth[/token|/signup|/signupanonym|/socialsignin|/magiclogin|/migrate]` ·
Lists: `v2/bringlists/{uuid}[/users]`, `bringlists/{uuid}[/details|/invitations|/users/{user}[/members/{public}]]`, `bringusers/{uuid}/lists` ·
Items: `v2/bringlists/{list}/items`, `v2/bringlistitemdetails[/{uuid}][/usersection|/usericon|/image|/assignedto]` ·
Users: `v2/bringusers/{uuid}`, `v2/bringusers/{uuid}/{locale|wallet[/vouchers|/loyaltycards]|userlocation|featuretoggles|mainfrontconfig|inspirations|inappmessages|featuretoggleconfigs|catalogextensions|bundles|appinvitations|yearinreview/{year}}`, `v2/bringusers/magiclink`, `bringusersettings/{uuid}[/{key}]`, `bringusers/{uuid}/features`, `v2/bringusers/profilepictures/{uuid}` ·
Commerce: `v1/providers[/{providerId}[/stores|/offersfront|/discountsfront]]`, `v1/offers[/brochures/{id}]`, `v1/discounts[/check|/mappings/*|/providers/{id}|/{uuid}]`, `v1/sponsoredproducts`, `v1/sectionleads`, `v1/promotedsections`, `v1/productspotlights`, `v1/specials/landingpage/{uuid}`, `v1/itempredictor/prediction`, `v1/users/{brn}/favourites/{providerId}[/{favourite}]` ·
Social/other: `v2/bringtemplates[/{uuid}][/image]`, `v2/bringinvitations[...], invitations`, `v2/bringappinvitations/...`, `v2/bringdeeplinks/shortdynamiclinks`, `v2/bringstatistics/...`, `v2/bringaudiences/{name}/{user}`, `v2/bringnotifications/lists/{list}`, `v2/premium/premiumfront/{user}`, `bringdevices`, `v2/bringdevices/geo`, `v1/events`, `trackings`, `v1/consents/{user}`, `v1/consentsfront/{user}`, `v2/bringuserprofiles/events/brochures`, `v1/bringusers/{user}/{specification|shopperdna|productrecommendations|personalizedsearch}/model`, `bringrecipes/parser`
