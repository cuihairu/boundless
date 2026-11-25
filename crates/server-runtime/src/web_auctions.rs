//! BW-14-03-04: the auction workflow.
//!
//! Ported from FantasyDemo's web front end
//! (`programming/fantasydemo/web/php/`) on top of [`crate::web_pages`]:
//!
//! - `Inventory.php:72-133` (the `action:auction` arm):
//!   `webCreateAuction` with the reference's own validation order - item
//!   selected, starting bid positive, buyout silently defaulting to -1,
//!   expiry positive - then the created-auction status and the redirect
//!   to `PlayerAuctions.php?auction_id=...`.
//! - `PlayerAuctions.php`: seller criteria + search + auction info, one
//!   row per pending auction with `timeFloatToString` expiries and `-`
//!   for missing buyouts.
//! - `SearchAuctions.php`: the criteria tree (item type / bid range /
//!   bidder, `AND`/`OR` combined), the result filtering (already bought
//!   out, expired), the seller-name lookups, the bid/buyout form posts
//!   and the always-redirect-after-action quirk.
//! - `Util.php:29-104`: [`time_float_to_string`], the expiry formatter
//!   with its `sec`/`min`/`hr`/`day` unit chain.
//!
//! The player entity keeps arriving through
//! [`crate::web_pages::WebEntityQueries`] (extended with
//! `webCreateAuction`/`webBidOnAuction`); the
//! `RemoteEntity("global_entities/AuctionHouse")` is a second entity, so
//! it gets its own [`AuctionHouseQueries`] seam, and the seller lookups
//! (`RemoteEntity("entities_by_id/Avatar/<dbid>")`) the [`AvatarLookup`]
//! seam.

use std::collections::BTreeMap;

use crate::web_pages::{
    AuctionSubmission, BidSubmission, DISCONNECT_PAGE_URL, InventoryItem, ItemTypeManager,
    PlayerInfo, WebEntityQueries, WebError, add_error_msg, add_status_msg, coerce_serial,
};
use crate::web_session::WebSession;

/// `BigWorld.php`'s `RemoteEntity("global_entities/AuctionHouse")`
/// (`PlayerAuctions.php:38`, `SearchAuctions.php:47`).
pub const AUCTION_HOUSE_PATH: &str = "global_entities/AuctionHouse";

/// `PlayerAuctions.php:110-113`: the empty-list paragraph (the
/// `Inventory` link markup is presentation-only).
pub const PLAYER_NO_AUCTIONS_MESSAGE: &str = "You have created no auctions that are currently pending. Go to Inventory to create an auction from one of your inventory items.";
/// `SearchAuctions.php:574`: the empty search paragraph.
pub const NO_AUCTIONS_FOUND_MESSAGE: &str = "No auctions found to match your criteria.";
/// `SearchAuctions.php:585-588`: the empty bid list paragraph.
pub const NO_BIDS_MESSAGE: &str = "You have made no bids. Search for items you wish to bid for, and either enter a maximum bid or, for nominated auctions, you may buy them out completely for the buyout price.";
/// `SearchAuctions.php:71`: the invalid bid amount error.
pub const BID_AMOUNT_INVALID_MESSAGE: &str = "Bid amount is invalid.";
/// `SearchAuctions.php:495`: the invalid selection error.
pub const AUCTION_SELECT_INVALID_MESSAGE: &str = "Selected auction is invalid.";
/// `Inventory.php:80-82`.
pub const NO_ITEM_SELECTED_MESSAGE: &str = "No item selected for auctioning.";
/// `Inventory.php:91`.
pub const INVALID_STARTING_BID_MESSAGE: &str = "Invalid starting bid price.";
/// `Inventory.php:103`.
pub const INVALID_EXPIRY_MESSAGE: &str = "Invalid expiry time.";
/// `Inventory.php:124`, with the item name spliced in.
pub const AUCTION_CREATED_FORMAT: &str = "Auction of {name} created.";
/// The bid status cell when this player is the highest bidder
/// (`SearchAuctions.php:757-762`); `Max bid: N` follows after a break.
pub const HIGHEST_BIDDER_MESSAGE: &str = "You are the highest bidder";
/// The bid status cell when someone outbid this player
/// (`SearchAuctions.php:763-766`).
pub const OUTBID_MESSAGE: &str = "You have been outbid";

/// `Util.php:29-104` with no granularity argument: render a duration as
/// a `sec`/`min`/`hr`/`day` chain, biggest unit last, zero modules not
/// prepended, negatives signed. The unit chain stops at days - a full
/// week renders as `0 sec`, faithfully.
pub fn time_float_to_string(time: f64) -> String {
    const UNITS: [(f64, &str); 4] = [(60.0, "sec"), (60.0, "min"), (24.0, "hr"), (7.0, "day")];

    let negative = time < 0.0;
    let mut time = time.abs();
    let mut out = String::new();

    for (value, name) in UNITS {
        // PHP casts the float division and remainder with (int).
        let div = (time / value).trunc();
        let module = (time - div * value).trunc();
        time = div;

        let mut unit = format!("{} {name}", module as i64);
        if module > 1.0 {
            unit.push('s');
        }

        if out.is_empty() {
            out = unit;
        } else if module != 0.0 {
            out = format!("{unit} {out}");
        }

        if div == 0.0 {
            break;
        }
    }

    if negative { format!("-{out}") } else { out }
}

/// PHP `(int)` on a string: the longest leading integer prefix, else 0.
pub(crate) fn php_str_to_int(raw: &str) -> i64 {
    let trimmed = raw.trim_start();
    let bytes = trimmed.as_bytes();
    let mut end = 0;
    if bytes.first() == Some(&b'+') || bytes.first() == Some(&b'-') {
        end = 1;
    }
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == digits_start {
        return 0;
    }
    match trimmed[..end].parse::<i64>() {
        Ok(value) => value,
        // PHP clamps to its integer bounds; the sign decides which.
        Err(_) => {
            if bytes[0] == b'-' {
                i64::MIN
            } else {
                i64::MAX
            }
        }
    }
}

/// PHP `urlencode`: alphanumerics and `-_.` pass through, a space
/// becomes `+`, everything else percent-encoded.
pub(crate) fn php_urlencode(raw: &str) -> String {
    let mut out = String::new();
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(byte as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// An opaque search-criteria handle: the PHP pages receive
/// `$res["criteria"]` and pass it straight back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Criteria(pub u64);

/// One `webGetAuctionInfo` entry, assembled from the fields the pages
/// read (`PlayerAuctions.php:130-147`, `SearchAuctions.php:290-303`,
/// `:739-771`).
#[derive(Debug, Clone, PartialEq)]
pub struct AuctionInfo {
    pub auction_id: u64,
    pub item_type: u32,
    /// The seller's lock handle on the item while it is auctioned.
    pub item_lock: i64,
    pub item_serial: u64,
    pub current_bid: u64,
    /// `None` when the auction has no buyout price.
    pub buyout_price: Option<u64>,
    /// The official-time stamp the auction expires at.
    pub expiry: f64,
    pub seller_dbid: u32,
    /// The database ids of everyone who has bid.
    pub bidders: Vec<u32>,
    pub highest_bidder: Option<u32>,
    /// The bidder's own maximum bid, shown back to the winner.
    pub current_max_bid: u64,
}

impl AuctionInfo {
    /// `SearchAuctions.php:298`: an auction is already bought out
    /// when `currentBid == buyoutPrice` - under PHP's `==` a missing
    /// buyout matches a zero bid.
    pub fn is_bought_out(&self) -> bool {
        match self.buyout_price {
            Some(buyout) => self.current_bid == buyout,
            None => self.current_bid == 0,
        }
    }
}

/// The `global_entities/AuctionHouse` method surface
/// (`PlayerAuctions.php`, `SearchAuctions.php`).
pub trait AuctionHouseQueries {
    /// `webGetTime` - the auction house's official clock.
    fn web_get_time(&self) -> Result<f64, WebError>;
    /// `webCreateSellerCriteria(array("dbID" => ...))`.
    fn web_create_seller_criteria(&self, db_id: u32) -> Result<Criteria, WebError>;
    /// `webCreateBidderCriteria(array("dbID" => ...))`.
    fn web_create_bidder_criteria(&self, db_id: u32) -> Result<Criteria, WebError>;
    /// `webCreateItemTypeCriteria(array("itemTypes" => ...))`.
    fn web_create_item_type_criteria(&self, item_types: &[u32]) -> Result<Criteria, WebError>;
    /// `webCreateBidRangeCriteria(array("minBid" => ..., "maxBid" => ...))`.
    fn web_create_bid_range_criteria(
        &self,
        min_bid: i64,
        max_bid: i64,
    ) -> Result<Criteria, WebError>;
    /// `webCombineAnd(array("criteria1" => ..., "criteria2" => ...))`.
    fn web_combine_and(&self, first: Criteria, second: Criteria) -> Result<Criteria, WebError>;
    /// `webCombineOr(array("criteria1" => ..., "criteria2" => ...))`.
    fn web_combine_or(&self, first: Criteria, second: Criteria) -> Result<Criteria, WebError>;
    /// `webSearchAuctions(array("criteria" => ...))` - `None` retrieves
    /// all (`SearchAuctions.php:247-249`).
    fn web_search_auctions(&self, criteria: Option<Criteria>) -> Result<Vec<u64>, WebError>;
    /// `webGetAuctionInfo(array("auctions" => ...))`.
    fn web_get_auction_info(&self, auctions: &[u64]) -> Result<Vec<AuctionInfo>, WebError>;
}

/// `RemoteEntity("entities_by_id/Avatar/<dbid>")` lookups
/// (`SearchAuctions.php:334-352`).
pub trait AvatarLookup {
    fn web_get_player_info(&self, dbid: u32) -> Result<PlayerInfo, WebError>;
}

/// One row of the `My Auctions` table (`PlayerAuctions.php:128-156`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerAuctionRow {
    pub auction_id: u64,
    /// The catalog name and icon, `None` for an id outside the catalog.
    pub item_type_name: Option<String>,
    pub icon_img_url: Option<String>,
    /// `timeFloatToString(expiry - officialTime)`.
    pub expiry_line: String,
    pub current_bid: u64,
    /// The rendered buyout cell: `None` (or zero) is `-`
    /// (`PlayerAuctions.php:146-147`).
    pub buyout_cell: Option<u64>,
}

/// The `PlayerAuctions.php` render data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerAuctionsRender {
    /// The carried-through `availableGold` - the page stores it but
    /// never reads it (`PlayerAuctions.php:46`).
    pub available_gold: u64,
    /// One row per pending auction; empty renders
    /// [`PLAYER_NO_AUCTIONS_MESSAGE`].
    pub rows: Vec<PlayerAuctionRow>,
}

/// `PlayerAuctionsPage` (`PlayerAuctions.php:34-92`). Any failure on
/// the setup legs disconnects; a search failure only reports.
pub fn player_auctions_page(
    session: &mut WebSession,
    player: &dyn WebEntityQueries,
    auction_house: &dyn AuctionHouseQueries,
    manager: &ItemTypeManager,
) -> Result<PlayerAuctionsRender, String> {
    // PlayerAuctions.php:40-55: the three setup queries share one catch
    // that disconnects.
    let player_dbid = match player.web_get_player_info() {
        Ok(info) => info.database_id,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };
    let available_gold = match player.web_get_gold_and_inventory() {
        Ok(reply) => reply.gold_pieces,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };
    let official_time = match auction_house.web_get_time() {
        Ok(time) => time,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };

    // PlayerAuctions.php:61-91: seller criteria, search, info. The
    // criteria and search legs catch; the info leg has no catch in the
    // reference (an uncaught exception is a PHP error page) - the port
    // disconnects defensively.
    let seller_criteria = match auction_house.web_create_seller_criteria(player_dbid) {
        Ok(criteria) => criteria,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };
    let searched = match auction_house.web_search_auctions(Some(seller_criteria)) {
        Ok(searched) => searched,
        Err(err) => {
            // PlayerAuctions.php:83-87: this leg reports without
            // disconnecting; the page renders with no auctions.
            add_error_msg(session, err.to_string());
            return Ok(PlayerAuctionsRender {
                available_gold,
                rows: Vec::new(),
            });
        }
    };
    // SearchAuctions.php:153-162: same report-and-return shape.
    let infos = match auction_house.web_get_auction_info(&searched) {
        Ok(infos) => infos,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };

    let rows = infos
        .into_iter()
        .map(|info| {
            let item_type = manager.get(info.item_type);
            PlayerAuctionRow {
                auction_id: info.auction_id,
                item_type_name: item_type.map(|item| item.name.clone()),
                icon_img_url: item_type.map(|item| item.icon_img_url.clone()),
                expiry_line: time_float_to_string(info.expiry - official_time),
                current_bid: info.current_bid,
                // `== FALSE` also matches a zero buyout.
                buyout_cell: info.buyout_price.filter(|buyout| *buyout != 0),
            }
        })
        .collect();

    Ok(PlayerAuctionsRender {
        available_gold,
        rows,
    })
}

/// The `Inventory.php` `action:auction` form (`$_REQUEST`).
#[derive(Debug, Default, Clone, Copy)]
pub struct AuctionFormRequest<'a> {
    pub item_select: Option<&'a str>,
    pub auction_bid_price: Option<&'a str>,
    pub auction_buyout: Option<&'a str>,
    pub auction_expiry: Option<&'a str>,
}

/// What one `action:auction` post produced (`Inventory.php:72-133`).
#[derive(Debug, Clone, PartialEq)]
pub enum CreateAuctionOutcome {
    /// No `item_select`: the status message is added and the page
    /// renders on (`Inventory.php:78-83`).
    NoItemSelected,
    /// `Invalid starting bid price.` (`Inventory.php:88-93`).
    InvalidBid,
    /// `Invalid expiry time.` (`Inventory.php:100-105`).
    InvalidExpiry,
    /// The entity refused: the message is in the session flash, no
    /// redirect (`Inventory.php:115-119`).
    Failed,
    /// The serial is not in the fetched inventory: PHP would fatal on
    /// the null item *after* creating the auction
    /// (`Inventory.php:122`); the port is defensive and the auction is
    /// already submitted.
    UnknownItem,
    /// Created: the status message is in the flash and the page
    /// redirects to `PlayerAuctions.php?auction_id=<id>`.
    Created { auction_id: u64, redirect: String },
}

/// `InventoryPage::initialise`'s auction arm (`Inventory.php:72-133`),
/// driven with the inventory the page fetched (pass
/// [`crate::web_pages::InventoryRender`]'s `items` through). The
/// auction is submitted before the item name is resolved - the
/// reference order (`Inventory.php:107-124`).
pub fn inventory_auction_action(
    session: &mut WebSession,
    entity: &mut dyn WebEntityQueries,
    manager: &ItemTypeManager,
    items: &BTreeMap<u64, InventoryItem>,
    request: AuctionFormRequest<'_>,
) -> CreateAuctionOutcome {
    let Some(raw_serial) = request.item_select else {
        add_status_msg(session, NO_ITEM_SELECTED_MESSAGE.to_string());
        return CreateAuctionOutcome::NoItemSelected;
    };
    // PHP casts to int up front; a cast of 0 still passes `isset` and
    // submits.
    let item_serial = php_str_to_int(raw_serial) as u64;

    let bid_price = request.auction_bid_price.map_or(0, php_str_to_int);
    if bid_price <= 0 {
        add_error_msg(session, INVALID_STARTING_BID_MESSAGE.to_string());
        return CreateAuctionOutcome::InvalidBid;
    }

    // A missing or non-positive buyout silently becomes -1
    // (`Inventory.php:94-98`).
    let buyout = request.auction_buyout.map_or(-1, php_str_to_int);
    let buyout = if buyout <= 0 { -1 } else { buyout };

    let expiry = request.auction_expiry.map_or(0, php_str_to_int);
    if expiry <= 0 {
        add_error_msg(session, INVALID_EXPIRY_MESSAGE.to_string());
        return CreateAuctionOutcome::InvalidExpiry;
    }

    let submission = AuctionSubmission {
        item_serial,
        expiry: expiry as u64,
        start_bid: bid_price as u64,
        buyout,
    };
    let auction_id = match entity.web_create_auction(submission) {
        Ok(auction_id) => auction_id,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return CreateAuctionOutcome::Failed;
        }
    };

    // Inventory.php:122-124: the item name for the status message comes
    // from the fetched inventory through the catalog.
    let Some(item) = items.get(&item_serial) else {
        return CreateAuctionOutcome::UnknownItem;
    };
    let name = manager
        .get(item.item_type)
        .map(|item_type| item_type.name.clone())
        .unwrap_or_default();
    add_status_msg(session, AUCTION_CREATED_FORMAT.replace("{name}", &name));
    CreateAuctionOutcome::Created {
        auction_id,
        // urlencode of a numeric id is the id itself.
        redirect: format!("PlayerAuctions.php?auction_id={auction_id}"),
    }
}

/// The `SearchAuctions.php` form (`$_REQUEST`), the fields the port
/// reads.
#[derive(Debug, Default, Clone, Copy)]
pub struct SearchRequest<'a> {
    pub search_type_name: Option<&'a str>,
    pub search_min_bid: Option<&'a str>,
    pub search_max_bid: Option<&'a str>,
    /// The `search_submit` button: an empty criteria retrieves all.
    pub search_submit: bool,
    /// `action:bid` / `action:buyout`.
    pub action: Option<&'a str>,
    pub auction_select: Option<&'a str>,
    pub bid_amount: Option<&'a str>,
}

/// One row of the search-results table (`SearchAuctions.php:651-733`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuctionRow {
    pub auction_id: u64,
    pub item_type_name: Option<String>,
    pub icon_img_url: Option<String>,
    pub seller_name: String,
    pub expiry_line: String,
    pub current_bid: u64,
    /// `currentBid > availableGold` - the radio is disabled and the bid
    /// cell renders bad (`SearchAuctions.php:700-711`).
    pub affordable: bool,
    /// `None` renders the `-` cell.
    pub buyout: Option<u64>,
}

/// The status cell of a bid row (`SearchAuctions.php:757-771`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BidStatus {
    /// `You are the highest bidder` plus `Max bid: <currentMaxBid>`.
    HighestBidder { max_bid: u64 },
    /// `You have been outbid`.
    Outbid,
    /// The `&nbsp;` cell - unreachable through the search flow (bid
    /// rows only exist for auctions the player bid on), kept for the
    /// reference's three-arm shape.
    #[allow(dead_code)]
    Other,
}

/// One row of the `My Bids` table (`SearchAuctions.php:738-790`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidRow {
    pub auction_id: u64,
    pub item_type_name: Option<String>,
    pub icon_img_url: Option<String>,
    pub seller_name: String,
    pub expiry_line: String,
    pub current_bid: u64,
    pub buyout: Option<u64>,
    pub status: BidStatus,
}

/// The `SearchAuctions.php` render data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRender {
    /// The results still open to this player.
    pub results: Vec<AuctionRow>,
    /// The auctions this player has bid on.
    pub bids: Vec<BidRow>,
    /// `search_submit` with no results renders
    /// [`NO_AUCTIONS_FOUND_MESSAGE`] (`SearchAuctions.php:575-580`).
    pub no_results_message: bool,
}

/// What one `SearchAuctions.php` request produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    /// After a bid/buyout post the page always redirects back to the
    /// searched result set (`SearchAuctions.php:486-516`) - even a
    /// failed bid's disconnect redirect is overwritten by this one.
    Redirect(String),
    Render(SearchRender),
}

/// One search result between the filter and the render.
struct Entry {
    info: AuctionInfo,
    seller_name: String,
}

/// The split `doSearchAction` produces: the results stay open to the
/// player, the bids are theirs. On the seller-lookup failure path the
/// split never ran - everything sits in `results`, `bids` is empty.
struct Split {
    results: Vec<Entry>,
    bids: Vec<Entry>,
}

/// `SearchAuctionsPage::initialise` (`SearchAuctions.php:436-521`) with
/// `doBidAction`/`doBuyoutAction` (`:55-110`) and `doSearchAction`
/// (`:114-373`). `Err` is the disconnect redirect.
pub fn search_auctions_page(
    session: &mut WebSession,
    player: &mut dyn WebEntityQueries,
    auction_house: &dyn AuctionHouseQueries,
    seller: &dyn AvatarLookup,
    manager: &ItemTypeManager,
    request: SearchRequest<'_>,
) -> Result<SearchOutcome, String> {
    // SearchAuctions.php:439-455: player info failure disconnects. The
    // gold query has no catch in the reference (an uncaught exception);
    // the port disconnects defensively.
    let player_dbid = match player.web_get_player_info() {
        Ok(info) => info.database_id,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };
    let available_gold = match player.web_get_gold_and_inventory() {
        Ok(reply) => reply.gold_pieces,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };

    // SearchAuctions.php:469-516: the bid/buyout actions always end in
    // the search-params redirect.
    if let Some(action) = request.action
        && (action == "bid" || action == "buyout")
    {
        if request.auction_select.is_none_or(|raw| raw.is_empty()) {
            add_error_msg(session, AUCTION_SELECT_INVALID_MESSAGE.to_string());
        }
        if action == "bid" {
            do_bid_action(session, player, request);
        } else {
            do_buyout_action(session, player, request);
        }
        return Ok(SearchOutcome::Redirect(search_params_redirect(&request)));
    }

    let split = do_search_action(
        session,
        player_dbid,
        auction_house,
        seller,
        manager,
        &request,
    )?;

    // SearchAuctions.php:517-521: the official clock for the render -
    // the in-loop stamps only did the expiry filtering.
    let official_time = match auction_house.web_get_time() {
        Ok(time) => time,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };

    Ok(SearchOutcome::Render(build_render(
        split,
        player_dbid,
        available_gold,
        official_time,
        manager,
        &request,
    )))
}

/// `doBidAction` (`SearchAuctions.php:55-81`). The caller still
/// redirects to the search params - even this leg's disconnect redirect
/// is overwritten.
fn do_bid_action(
    session: &mut WebSession,
    player: &mut dyn WebEntityQueries,
    request: SearchRequest<'_>,
) {
    let bid_amount = request.bid_amount.map_or(0, php_str_to_int);
    if bid_amount <= 0 {
        add_error_msg(session, BID_AMOUNT_INVALID_MESSAGE.to_string());
        return;
    }
    let submission = BidSubmission {
        auction_id: request.auction_select.and_then(coerce_serial).unwrap_or(0),
        is_buyout: false,
        bid_amount: bid_amount as u64,
    };
    match player.web_bid_on_auction(submission) {
        Ok(status) => add_status_msg(session, status),
        Err(err) => {
            // SearchAuctions.php:75-79: addExceptionMsgAndDisconnect -
            // whose redirect the caller overwrites.
            add_error_msg(session, err.to_string());
        }
    }
}

/// `doBuyoutAction` (`SearchAuctions.php:85-110`): a buyout bids zero
/// with the buyout flag.
fn do_buyout_action(
    session: &mut WebSession,
    player: &mut dyn WebEntityQueries,
    request: SearchRequest<'_>,
) {
    let submission = BidSubmission {
        auction_id: request.auction_select.and_then(coerce_serial).unwrap_or(0),
        is_buyout: true,
        bid_amount: 0,
    };
    match player.web_bid_on_auction(submission) {
        Ok(status) => add_status_msg(session, status),
        Err(err) => add_error_msg(session, err.to_string()),
    }
}

/// `SearchAuctions.php:470-486`: the redirect carries the four search
/// parameters, always, each `urlencode`d (a missing one renders empty).
fn search_params_redirect(request: &SearchRequest<'_>) -> String {
    // A submit button carries no value, so the fourth parameter is
    // always the empty string.
    let param = |raw: Option<&str>| php_urlencode(raw.unwrap_or_default());
    format!(
        "SearchAuctions.php?search_type_name={}&search_min_bid={}&search_max_bid={}&search_submit=",
        param(request.search_type_name),
        param(request.search_min_bid),
        param(request.search_max_bid),
    )
}

/// `doSearchAction` (`SearchAuctions.php:114-373`). `Err` is the
/// disconnect redirect - only the item-type criteria leg disconnects;
/// every other failure reports and renders with what arrived.
fn do_search_action(
    session: &mut WebSession,
    player_dbid: u32,
    auction_house: &dyn AuctionHouseQueries,
    seller: &dyn AvatarLookup,
    manager: &ItemTypeManager,
    request: &SearchRequest<'_>,
) -> Result<Split, String> {
    // The item-type leg: `eregi` over the catalog names.
    let mut search_criteria: Option<Criteria> = None;
    if let Some(type_name) = request.search_type_name.filter(|raw| !raw.is_empty()) {
        let item_types: Vec<u32> = manager
            .item_type_ids()
            .filter(|id| {
                manager
                    .get(*id)
                    .map(|item| name_matches(&item.name, type_name))
                    .unwrap_or(false)
            })
            .collect();
        let item_type_criteria = match auction_house.web_create_item_type_criteria(&item_types) {
            Ok(criteria) => criteria,
            Err(err) => {
                // SearchAuctions.php:162-167: the only disconnecting
                // leg inside the search.
                add_error_msg(session, err.to_string());
                return Err(DISCONNECT_PAGE_URL.to_string());
            }
        };
        // First criteria in the chain: the reference's combine arm is
        // dead here - `$criteria1` is still null at this point.
        search_criteria = Some(item_type_criteria);
    }

    // The bid-range leg: an empty string is -1 (unset), anything else
    // casts through PHP's `(int)`.
    let search_min_bid = bid_bound(request.search_min_bid);
    let search_max_bid = bid_bound(request.search_max_bid);
    if search_min_bid != -1 || search_max_bid != -1 {
        let bid_range_criteria =
            match auction_house.web_create_bid_range_criteria(search_min_bid, search_max_bid) {
                Ok(criteria) => criteria,
                Err(err) => {
                    add_error_msg(session, err.to_string());
                    return Ok(Split {
                        results: Vec::new(),
                        bids: Vec::new(),
                    });
                }
            };
        search_criteria = Some(match search_criteria {
            None => bid_range_criteria,
            Some(existing) => match auction_house.web_combine_and(existing, bid_range_criteria) {
                Ok(criteria) => criteria,
                Err(err) => {
                    add_error_msg(session, err.to_string());
                    return Ok(Split {
                        results: Vec::new(),
                        bids: Vec::new(),
                    });
                }
            },
        });
    }

    let bidder_criteria = match auction_house.web_create_bidder_criteria(player_dbid) {
        Ok(criteria) => criteria,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Ok(Split {
                results: Vec::new(),
                bids: Vec::new(),
            });
        }
    };

    // No typed criteria: `search_submit` retrieves all, anything else
    // narrows to the player's own bids.
    let final_criteria = match search_criteria {
        None => {
            if request.search_submit {
                None
            } else {
                Some(bidder_criteria)
            }
        }
        Some(criteria) => match auction_house.web_combine_or(bidder_criteria, criteria) {
            Ok(combined) => Some(combined),
            Err(err) => {
                add_error_msg(session, err.to_string());
                return Ok(Split {
                    results: Vec::new(),
                    bids: Vec::new(),
                });
            }
        },
    };

    // SearchAuctions.php:138-147: this leg reports and returns (the
    // reference leaves the disconnect arm commented out as a TODO).
    let searched = match auction_house.web_search_auctions(final_criteria) {
        Ok(searched) => searched,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Ok(Split {
                results: Vec::new(),
                bids: Vec::new(),
            });
        }
    };
    // SearchAuctions.php:153-162: same report-and-return shape.
    let infos = match auction_house.web_get_auction_info(&searched) {
        Ok(infos) => infos,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Ok(Split {
                results: Vec::new(),
                bids: Vec::new(),
            });
        }
    };

    // SearchAuctions.php:289-310: the already-bought-out and expired
    // auctions drop out. `webGetTime` runs inside the loop in the
    // reference; an uncaught failure would be a PHP error page, the
    // port keeps the previous stamp.
    let mut official_time = 0.0;
    let mut entries: Vec<Entry> = Vec::new();
    for info in infos {
        if info.is_bought_out() {
            continue;
        }
        if let Ok(time) = auction_house.web_get_time() {
            official_time = time;
        }
        if info.expiry < official_time {
            continue;
        }
        entries.push(Entry {
            info,
            seller_name: String::new(),
        });
    }

    // SearchAuctions.php:325-372: resolve the seller names, then split
    // the player's own bids out and drop their own sales. A seller
    // lookup failure reports and keeps everything unresolved - the
    // reference returns without running the split.
    let mut seller_names: BTreeMap<u32, String> = BTreeMap::new();
    for entry in &mut entries {
        let seller_dbid = entry.info.seller_dbid;
        let name = match seller_names.get(&seller_dbid) {
            Some(name) => name.clone(),
            None => match seller.web_get_player_info(seller_dbid) {
                Ok(info) => {
                    seller_names.insert(seller_dbid, info.player_name.clone());
                    info.player_name
                }
                Err(err) => {
                    add_error_msg(session, err.to_string());
                    return Ok(Split {
                        results: entries,
                        bids: Vec::new(),
                    });
                }
            },
        };
        entry.seller_name = name;
    }

    // SearchAuctions.php:352-371: bids move out; the player's own sales
    // drop entirely.
    let mut bids = Vec::new();
    let mut results = Vec::new();
    for entry in entries {
        if entry.info.bidders.contains(&player_dbid) {
            bids.push(entry);
        } else if entry.info.seller_dbid != player_dbid {
            results.push(entry);
        }
    }

    Ok(Split { results, bids })
}

/// The bid-range bound for one form field (`SearchAuctions.php:186-201`).
fn bid_bound(raw: Option<&str>) -> i64 {
    match raw {
        None => -1,
        Some("") => -1,
        Some(raw) => php_str_to_int(raw),
    }
}

/// The render pass: rows off the split, timed with the post-search
/// official clock (`SearchAuctions.php:517-521`, `:651-790`).
fn build_render(
    split: Split,
    player_dbid: u32,
    available_gold: u64,
    official_time: f64,
    manager: &ItemTypeManager,
    request: &SearchRequest<'_>,
) -> SearchRender {
    let names = |info: &AuctionInfo| {
        (
            manager.get(info.item_type).map(|item| item.name.clone()),
            manager
                .get(info.item_type)
                .map(|item| item.icon_img_url.clone()),
        )
    };

    let results: Vec<_> = split
        .results
        .into_iter()
        .map(|entry| {
            let (name, icon) = names(&entry.info);
            AuctionRow {
                auction_id: entry.info.auction_id,
                item_type_name: name,
                icon_img_url: icon,
                seller_name: entry.seller_name,
                expiry_line: time_float_to_string(entry.info.expiry - official_time),
                current_bid: entry.info.current_bid,
                affordable: entry.info.current_bid <= available_gold,
                buyout: entry.info.buyout_price,
            }
        })
        .collect();

    let bids = split
        .bids
        .into_iter()
        .map(|entry| {
            let (name, icon) = names(&entry.info);
            BidRow {
                auction_id: entry.info.auction_id,
                item_type_name: name,
                icon_img_url: icon,
                seller_name: entry.seller_name,
                expiry_line: time_float_to_string(entry.info.expiry - official_time),
                current_bid: entry.info.current_bid,
                buyout: entry.info.buyout_price,
                status: bid_status(&entry.info, player_dbid),
            }
        })
        .collect();

    // `SearchAuctions.php` prints "No auctions found." only for an
    // explicit search submit; a parameterized view of someone else's
    // auctions stays silent.
    let no_results_message = results.is_empty() && request.search_submit;

    SearchRender {
        no_results_message,
        results,
        bids,
    }
}

/// The bid-row status cell (`SearchAuctions.php:757-771`): the highest
/// bidder sees their max bid, every other bidder has been outbid - the
/// reference's third arm (`&nbsp;`) is unreachable, bid rows only exist
/// for auctions the player bid on.
fn bid_status(info: &AuctionInfo, player_dbid: u32) -> BidStatus {
    if info.highest_bidder == Some(player_dbid) {
        BidStatus::HighestBidder {
            max_bid: info.current_max_bid,
        }
    } else {
        BidStatus::Outbid
    }
}

/// `eregi($searchTypeName, $itemName)` for the plain-word searches the
/// sample flows use: a case-insensitive substring test. POSIX regex
/// alternations and classes are not modelled.
fn name_matches(item_name: &str, type_name: &str) -> bool {
    item_name.to_lowercase().contains(&type_name.to_lowercase())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    use super::*;
    use crate::web_pages::{
        BW_ITEM_TYPE_SWORD, FLASH_KEY_ERR_MSGS, FLASH_KEY_STATUS_MSGS, GoldAndInventory,
    };

    const PLAYER_DBID: u32 = 7;

    fn player_info() -> PlayerInfo {
        PlayerInfo {
            char_class: "ranger".to_string(),
            database_id: PLAYER_DBID,
            player_name: "Boris".to_string(),
            health: 100,
            max_health: 100,
            frags: 0,
            position: [1.0, 2.0, 3.0],
            direction: [0.0, 0.0, 90.0],
            online: true,
        }
    }

    fn gold(gold_pieces: u64) -> GoldAndInventory {
        GoldAndInventory {
            gold_pieces,
            inventory_items: Vec::new(),
            locked_items: Vec::new(),
        }
    }

    fn auction(id: u64) -> AuctionInfo {
        AuctionInfo {
            auction_id: id,
            item_type: BW_ITEM_TYPE_SWORD,
            item_lock: -1,
            item_serial: id * 10,
            current_bid: 50,
            buyout_price: Some(500),
            expiry: 1500.0,
            seller_dbid: 99,
            bidders: Vec::new(),
            highest_bidder: None,
            current_max_bid: 0,
        }
    }

    /// The `global_entities/AuctionHouse` stub: canned replies per leg,
    /// every call recorded in order. `time_failures` makes the first
    /// `webGetTime` calls fail (the in-loop clock vs. the render clock).
    struct StubHouse {
        time: Result<f64, String>,
        time_failures: Cell<usize>,
        time_calls: Cell<usize>,
        seller_criteria: Result<u64, String>,
        bidder_criteria: Result<u64, String>,
        item_type_criteria: Result<u64, String>,
        bid_range_criteria: Result<u64, String>,
        combine_and: Result<u64, String>,
        combine_or: Result<u64, String>,
        searched: Result<Vec<u64>, String>,
        infos: Result<Vec<AuctionInfo>, String>,
        calls: RefCell<Vec<String>>,
    }

    impl Default for StubHouse {
        fn default() -> Self {
            Self {
                time: Ok(1000.0),
                time_failures: Cell::new(0),
                time_calls: Cell::new(0),
                seller_criteria: Ok(4),
                bidder_criteria: Ok(5),
                item_type_criteria: Ok(1),
                bid_range_criteria: Ok(2),
                combine_and: Ok(3),
                combine_or: Ok(6),
                searched: Ok(Vec::new()),
                infos: Ok(Vec::new()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl StubHouse {
        fn with_infos(infos: Vec<AuctionInfo>) -> Self {
            let ids = infos.iter().map(|info| info.auction_id).collect();
            Self {
                searched: Ok(ids),
                infos: Ok(infos),
                ..Self::default()
            }
        }

        fn take_calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl AuctionHouseQueries for StubHouse {
        fn web_get_time(&self) -> Result<f64, WebError> {
            let call = self.time_calls.get();
            self.time_calls.set(call + 1);
            self.calls.borrow_mut().push("time".to_string());
            if call < self.time_failures.get() {
                return Err(WebError("clock down".to_string()));
            }
            self.time.clone().map_err(WebError)
        }

        fn web_create_seller_criteria(&self, db_id: u32) -> Result<Criteria, WebError> {
            self.calls
                .borrow_mut()
                .push(format!("create_seller({db_id})"));
            self.seller_criteria.clone().map(Criteria).map_err(WebError)
        }

        fn web_create_bidder_criteria(&self, db_id: u32) -> Result<Criteria, WebError> {
            self.calls
                .borrow_mut()
                .push(format!("create_bidder({db_id})"));
            self.bidder_criteria.clone().map(Criteria).map_err(WebError)
        }

        fn web_create_item_type_criteria(&self, item_types: &[u32]) -> Result<Criteria, WebError> {
            self.calls
                .borrow_mut()
                .push(format!("create_item_type({item_types:?})"));
            self.item_type_criteria
                .clone()
                .map(Criteria)
                .map_err(WebError)
        }

        fn web_create_bid_range_criteria(
            &self,
            min_bid: i64,
            max_bid: i64,
        ) -> Result<Criteria, WebError> {
            self.calls
                .borrow_mut()
                .push(format!("create_bid_range({min_bid}, {max_bid})"));
            self.bid_range_criteria
                .clone()
                .map(Criteria)
                .map_err(WebError)
        }

        fn web_combine_and(&self, first: Criteria, second: Criteria) -> Result<Criteria, WebError> {
            self.calls
                .borrow_mut()
                .push(format!("combine_and({}, {})", first.0, second.0));
            self.combine_and.clone().map(Criteria).map_err(WebError)
        }

        fn web_combine_or(&self, first: Criteria, second: Criteria) -> Result<Criteria, WebError> {
            self.calls
                .borrow_mut()
                .push(format!("combine_or({}, {})", first.0, second.0));
            self.combine_or.clone().map(Criteria).map_err(WebError)
        }

        fn web_search_auctions(&self, criteria: Option<Criteria>) -> Result<Vec<u64>, WebError> {
            self.calls
                .borrow_mut()
                .push(format!("search({criteria:?})"));
            self.searched.clone().map_err(WebError)
        }

        fn web_get_auction_info(&self, auctions: &[u64]) -> Result<Vec<AuctionInfo>, WebError> {
            self.calls.borrow_mut().push(format!("info({auctions:?})"));
            self.infos.clone().map_err(WebError)
        }
    }

    /// The `entities_by_id/Avatar/<dbid>` stub with per-seller replies.
    struct StubSeller {
        replies: BTreeMap<u32, Result<PlayerInfo, String>>,
        lookups: RefCell<Vec<u32>>,
    }

    impl StubSeller {
        fn named(dbid: u32, name: &str) -> Self {
            let mut info = player_info();
            info.database_id = dbid;
            info.player_name = name.to_string();
            Self {
                replies: BTreeMap::from([(dbid, Ok(info))]),
                lookups: RefCell::new(Vec::new()),
            }
        }

        fn failing(dbid: u32) -> Self {
            Self {
                replies: BTreeMap::from([(dbid, Err("avatar gone".to_string()))]),
                lookups: RefCell::new(Vec::new()),
            }
        }
    }

    impl AvatarLookup for StubSeller {
        fn web_get_player_info(&self, dbid: u32) -> Result<PlayerInfo, WebError> {
            self.lookups.borrow_mut().push(dbid);
            self.replies
                .get(&dbid)
                .cloned()
                .unwrap_or_else(|| Ok(player_info()))
                .map_err(WebError)
        }
    }

    /// The player entity stub: records auction/bid submissions.
    struct StubPlayer {
        player: Result<PlayerInfo, String>,
        inventory: Result<GoldAndInventory, String>,
        bid_reply: Result<String, String>,
        create_reply: Result<u64, String>,
        created: RefCell<Vec<AuctionSubmission>>,
        bids: RefCell<Vec<BidSubmission>>,
    }

    impl Default for StubPlayer {
        fn default() -> Self {
            Self {
                player: Ok(player_info()),
                inventory: Ok(gold(250)),
                bid_reply: Ok("Your bid has been placed".to_string()),
                create_reply: Ok(33),
                created: RefCell::new(Vec::new()),
                bids: RefCell::new(Vec::new()),
            }
        }
    }

    impl StubPlayer {
        fn failing_player() -> Self {
            Self {
                player: Err("session dead".to_string()),
                ..Self::default()
            }
        }

        fn failing_gold() -> Self {
            Self {
                inventory: Err("gold gone".to_string()),
                ..Self::default()
            }
        }

        fn failing_create() -> Self {
            Self {
                create_reply: Err("house refused".to_string()),
                ..Self::default()
            }
        }

        fn failing_bid() -> Self {
            Self {
                bid_reply: Err("bid refused".to_string()),
                ..Self::default()
            }
        }

        fn take_created(&self) -> Vec<AuctionSubmission> {
            self.created.borrow().clone()
        }

        fn take_bids(&self) -> Vec<BidSubmission> {
            self.bids.borrow().clone()
        }
    }

    impl WebEntityQueries for StubPlayer {
        fn web_create_character(&mut self, _name: &str) -> Result<(), WebError> {
            unreachable!("the auction pages never create characters")
        }

        fn web_choose_character(&mut self, _name: &str) -> Result<(String, u32), WebError> {
            unreachable!("the auction pages never choose characters")
        }

        fn web_get_character_list(
            &self,
        ) -> Result<Vec<crate::web_pages::CharacterSummary>, WebError> {
            unreachable!("the auction pages never list characters")
        }

        fn web_get_player_info(&self) -> Result<PlayerInfo, WebError> {
            self.player.clone().map_err(WebError)
        }

        fn web_get_gold_and_inventory(&self) -> Result<GoldAndInventory, WebError> {
            self.inventory.clone().map_err(WebError)
        }

        fn web_create_auction(&mut self, request: AuctionSubmission) -> Result<u64, WebError> {
            self.created.borrow_mut().push(request);
            self.create_reply.clone().map_err(WebError)
        }

        fn web_bid_on_auction(&mut self, request: BidSubmission) -> Result<String, WebError> {
            self.bids.borrow_mut().push(request);
            self.bid_reply.clone().map_err(WebError)
        }
    }

    fn take_errors(session: &mut WebSession) -> Vec<String> {
        session.take_flash(FLASH_KEY_ERR_MSGS).unwrap_or_default()
    }

    fn take_status(session: &mut WebSession) -> Vec<String> {
        session
            .take_flash(FLASH_KEY_STATUS_MSGS)
            .unwrap_or_default()
    }

    fn inventory_with(serial: u64, item_type: u32) -> BTreeMap<u64, InventoryItem> {
        BTreeMap::from([(
            serial,
            InventoryItem {
                serial,
                item_type,
                lock_handle: -1,
            },
        )])
    }

    #[test]
    fn time_float_to_string_pins_the_unit_chain() {
        assert_eq!(time_float_to_string(0.0), "0 sec");
        assert_eq!(time_float_to_string(45.0), "45 secs");
        assert_eq!(time_float_to_string(60.0), "1 min 0 sec");
        assert_eq!(time_float_to_string(90.0), "1 min 30 secs");
        assert_eq!(time_float_to_string(120.0), "2 mins 0 sec");
        assert_eq!(time_float_to_string(3600.0), "1 hr 0 sec");
        assert_eq!(time_float_to_string(86401.0), "1 day 1 sec");
        // A full week overflows the day unit and renders as zero.
        assert_eq!(time_float_to_string(604800.0), "0 sec");
        assert_eq!(time_float_to_string(-90.0), "-1 min 30 secs");
    }

    #[test]
    fn php_str_to_int_casts_like_php() {
        assert_eq!(php_str_to_int("42"), 42);
        assert_eq!(php_str_to_int(" 7x"), 7);
        assert_eq!(php_str_to_int("+5"), 5);
        assert_eq!(php_str_to_int("-3"), -3);
        assert_eq!(php_str_to_int("abc"), 0);
        assert_eq!(php_str_to_int("5.9"), 5);
        assert_eq!(php_str_to_int("99999999999999999999"), i64::MAX);
        assert_eq!(php_str_to_int("-99999999999999999999"), i64::MIN);
    }

    #[test]
    fn php_urlencode_encodes_like_php() {
        assert_eq!(php_urlencode("Staff of Lightning"), "Staff+of+Lightning");
        assert_eq!(php_urlencode("a-b_c.d"), "a-b_c.d");
        assert_eq!(php_urlencode("50%"), "50%25");
        assert_eq!(php_urlencode(""), "");
    }

    #[test]
    fn is_bought_out_matches_php_equality() {
        let mut info = auction(1);
        info.buyout_price = Some(500);
        info.current_bid = 500;
        assert!(info.is_bought_out());
        info.current_bid = 50;
        assert!(!info.is_bought_out());
        info.buyout_price = None;
        assert!(!info.is_bought_out());
        info.current_bid = 0;
        // PHP `==`: a missing buyout matches a zero bid.
        assert!(info.is_bought_out());
    }

    #[test]
    fn player_auctions_renders_one_row_per_pending_auction() {
        let mut no_buyout = auction(2);
        no_buyout.buyout_price = None;
        let mut zero_buyout = auction(3);
        zero_buyout.buyout_price = Some(0);
        let house = StubHouse::with_infos(vec![auction(1), no_buyout, zero_buyout]);
        let player = StubPlayer::default();
        let mut session = WebSession::new();
        let manager = crate::web_pages::default_item_type_manager();

        let render = player_auctions_page(&mut session, &player, &house, &manager).unwrap();

        assert_eq!(render.available_gold, 250);
        assert_eq!(render.rows.len(), 3);
        assert_eq!(render.rows[0].auction_id, 1);
        assert_eq!(render.rows[0].item_type_name.as_deref(), Some("Sword"));
        assert_eq!(
            render.rows[0].icon_img_url.as_deref(),
            Some("images/items/icon_sword2.gif")
        );
        assert_eq!(render.rows[0].expiry_line, "8 mins 20 secs");
        assert_eq!(render.rows[0].current_bid, 50);
        assert_eq!(render.rows[0].buyout_cell, Some(500));
        // Missing or zero buyout both render `-`.
        assert_eq!(render.rows[1].buyout_cell, None);
        assert_eq!(render.rows[2].buyout_cell, None);
        assert!(take_errors(&mut session).is_empty());
    }

    #[test]
    fn player_auctions_with_no_pending_auctions_renders_no_rows() {
        let house = StubHouse::default();
        let player = StubPlayer::default();
        let mut session = WebSession::new();

        let render = player_auctions_page(
            &mut session,
            &player,
            &house,
            &crate::web_pages::default_item_type_manager(),
        )
        .unwrap();

        assert!(render.rows.is_empty());
    }

    #[test]
    fn player_auctions_search_failure_reports_without_disconnecting() {
        let house = StubHouse {
            searched: Err("search blew up".to_string()),
            ..StubHouse::default()
        };
        let player = StubPlayer::default();
        let mut session = WebSession::new();

        let render = player_auctions_page(
            &mut session,
            &player,
            &house,
            &crate::web_pages::default_item_type_manager(),
        )
        .unwrap();

        assert!(render.rows.is_empty());
        assert_eq!(take_errors(&mut session), ["search blew up"]);
    }

    #[test]
    fn player_auctions_setup_legs_disconnect() {
        let manager = crate::web_pages::default_item_type_manager();
        let legs: [(&str, StubPlayer, StubHouse); 4] = [
            (
                "player info",
                StubPlayer::failing_player(),
                StubHouse::default(),
            ),
            ("gold", StubPlayer::failing_gold(), StubHouse::default()),
            (
                "clock",
                StubPlayer::default(),
                StubHouse {
                    time: Err("clock down".to_string()),
                    ..StubHouse::default()
                },
            ),
            (
                "seller criteria",
                StubPlayer::default(),
                StubHouse {
                    seller_criteria: Err("criteria refused".to_string()),
                    ..StubHouse::default()
                },
            ),
        ];
        for (leg, mut player, house) in legs {
            let mut session = WebSession::new();
            let outcome = player_auctions_page(
                &mut session,
                &mut player as &mut dyn WebEntityQueries,
                &house,
                &manager,
            );
            assert_eq!(outcome, Err(DISCONNECT_PAGE_URL.to_string()), "{leg}");
            let errors = take_errors(&mut session);
            assert_eq!(errors.len(), 1, "{leg}");
            assert!(!errors[0].is_empty(), "{leg}");
        }
    }

    #[test]
    fn player_auctions_info_failure_disconnects_defensively() {
        // The reference info leg has no catch (an uncaught exception
        // would be a PHP error page); the port disconnects.
        let house = StubHouse {
            infos: Err("info blew up".to_string()),
            ..StubHouse::default()
        };
        let player = StubPlayer::default();
        let mut session = WebSession::new();

        let outcome = player_auctions_page(
            &mut session,
            &player,
            &house,
            &crate::web_pages::default_item_type_manager(),
        );

        assert_eq!(outcome, Err(DISCONNECT_PAGE_URL.to_string()));
        assert_eq!(take_errors(&mut session), ["info blew up"]);
    }

    #[test]
    fn create_auction_requires_an_item() {
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();
        let manager = crate::web_pages::default_item_type_manager();

        let outcome = inventory_auction_action(
            &mut session,
            &mut player,
            &manager,
            &inventory_with(1000, BW_ITEM_TYPE_SWORD),
            AuctionFormRequest::default(),
        );

        assert_eq!(outcome, CreateAuctionOutcome::NoItemSelected);
        assert_eq!(take_status(&mut session), [NO_ITEM_SELECTED_MESSAGE]);
        assert!(player.take_created().is_empty());
    }

    #[test]
    fn create_auction_rejects_non_positive_bids() {
        let manager = crate::web_pages::default_item_type_manager();
        let items = inventory_with(1000, BW_ITEM_TYPE_SWORD);
        for bid in [None, Some("0"), Some("-3"), Some("abc")] {
            let mut player = StubPlayer::default();
            let mut session = WebSession::new();
            let outcome = inventory_auction_action(
                &mut session,
                &mut player,
                &manager,
                &items,
                AuctionFormRequest {
                    item_select: Some("1000"),
                    auction_bid_price: bid,
                    auction_buyout: Some("100"),
                    auction_expiry: Some("3600"),
                },
            );
            assert_eq!(outcome, CreateAuctionOutcome::InvalidBid, "{bid:?}");
            assert_eq!(
                take_errors(&mut session),
                [INVALID_STARTING_BID_MESSAGE],
                "{bid:?}"
            );
            assert!(player.take_created().is_empty(), "{bid:?}");
        }
    }

    #[test]
    fn create_auction_rejects_non_positive_expiry() {
        let manager = crate::web_pages::default_item_type_manager();
        let items = inventory_with(1000, BW_ITEM_TYPE_SWORD);
        for expiry in [None, Some("0")] {
            let mut player = StubPlayer::default();
            let mut session = WebSession::new();
            let outcome = inventory_auction_action(
                &mut session,
                &mut player,
                &manager,
                &items,
                AuctionFormRequest {
                    item_select: Some("1000"),
                    auction_bid_price: Some("10"),
                    auction_buyout: Some("100"),
                    auction_expiry: expiry,
                },
            );
            assert_eq!(outcome, CreateAuctionOutcome::InvalidExpiry, "{expiry:?}");
            assert_eq!(take_errors(&mut session), [INVALID_EXPIRY_MESSAGE]);
            assert!(player.take_created().is_empty());
        }
    }

    #[test]
    fn create_auction_defaults_the_buyout_to_minus_one() {
        let manager = crate::web_pages::default_item_type_manager();
        for buyout in [None, Some("0"), Some("-5")] {
            let mut player = StubPlayer::default();
            let mut session = WebSession::new();
            let outcome = inventory_auction_action(
                &mut session,
                &mut player,
                &manager,
                &inventory_with(1000, BW_ITEM_TYPE_SWORD),
                AuctionFormRequest {
                    item_select: Some("1000"),
                    auction_bid_price: Some("10"),
                    auction_buyout: buyout,
                    auction_expiry: Some("3600"),
                },
            );
            assert!(
                matches!(outcome, CreateAuctionOutcome::Created { .. }),
                "{buyout:?}"
            );
            assert_eq!(
                player.take_created(),
                [AuctionSubmission {
                    item_serial: 1000,
                    expiry: 3600,
                    start_bid: 10,
                    buyout: -1,
                }],
                "{buyout:?}"
            );
        }
    }

    #[test]
    fn create_auction_submits_before_resolving_the_name() {
        // Inventory.php:107-124: the auction is created first; only
        // then does the page look the item up. A serial outside the
        // fetched inventory leaves the auction submitted and the page
        // would fatal in PHP - the port reports the defensible shape.
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();
        let manager = crate::web_pages::default_item_type_manager();

        let outcome = inventory_auction_action(
            &mut session,
            &mut player,
            &manager,
            &inventory_with(1000, BW_ITEM_TYPE_SWORD),
            AuctionFormRequest {
                item_select: Some("99"),
                auction_bid_price: Some("10"),
                auction_buyout: Some("100"),
                auction_expiry: Some("3600"),
            },
        );

        assert_eq!(outcome, CreateAuctionOutcome::UnknownItem);
        assert_eq!(player.take_created().len(), 1);
        assert!(take_status(&mut session).is_empty());
    }

    #[test]
    fn create_auction_announces_the_item_name() {
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();
        let manager = crate::web_pages::default_item_type_manager();

        let outcome = inventory_auction_action(
            &mut session,
            &mut player,
            &manager,
            &inventory_with(1000, BW_ITEM_TYPE_SWORD),
            AuctionFormRequest {
                item_select: Some("1000"),
                auction_bid_price: Some("10"),
                auction_buyout: Some("250"),
                auction_expiry: Some("3600"),
            },
        );

        assert_eq!(
            outcome,
            CreateAuctionOutcome::Created {
                auction_id: 33,
                redirect: "PlayerAuctions.php?auction_id=33".to_string(),
            }
        );
        assert_eq!(take_status(&mut session), ["Auction of Sword created."]);
        assert_eq!(
            player.take_created(),
            [AuctionSubmission {
                item_serial: 1000,
                expiry: 3600,
                start_bid: 10,
                buyout: 250,
            }]
        );
    }

    #[test]
    fn create_auction_without_catalog_entry_renders_a_blank_name() {
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();
        let manager = crate::web_pages::default_item_type_manager();

        let outcome = inventory_auction_action(
            &mut session,
            &mut player,
            &manager,
            &inventory_with(1000, 999),
            AuctionFormRequest {
                item_select: Some("1000"),
                auction_bid_price: Some("10"),
                auction_buyout: None,
                auction_expiry: Some("3600"),
            },
        );

        assert!(matches!(outcome, CreateAuctionOutcome::Created { .. }));
        assert_eq!(take_status(&mut session), ["Auction of  created."]);
    }

    #[test]
    fn create_auction_reports_entity_refusal() {
        let mut player = StubPlayer::failing_create();
        let mut session = WebSession::new();
        let manager = crate::web_pages::default_item_type_manager();

        let outcome = inventory_auction_action(
            &mut session,
            &mut player,
            &manager,
            &inventory_with(1000, BW_ITEM_TYPE_SWORD),
            AuctionFormRequest {
                item_select: Some("1000"),
                auction_bid_price: Some("10"),
                auction_buyout: None,
                auction_expiry: Some("3600"),
            },
        );

        assert_eq!(outcome, CreateAuctionOutcome::Failed);
        assert_eq!(take_errors(&mut session), ["house refused"]);
    }

    #[test]
    fn bid_action_records_the_submission_and_redirects() {
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                action: Some("bid"),
                auction_select: Some("5"),
                bid_amount: Some("100"),
                ..SearchRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SearchOutcome::Redirect(
                "SearchAuctions.php?search_type_name=&search_min_bid=&search_max_bid=&search_submit="
                    .to_string()
            )
        );
        assert_eq!(
            player.take_bids(),
            [BidSubmission {
                auction_id: 5,
                is_buyout: false,
                bid_amount: 100,
            }]
        );
        assert_eq!(take_status(&mut session), ["Your bid has been placed"]);
    }

    #[test]
    fn bid_action_without_an_amount_is_rejected_but_still_redirects() {
        for amount in [None, Some("0"), Some("abc")] {
            let mut player = StubPlayer::default();
            let house = StubHouse::default();
            let mut session = WebSession::new();
            let outcome = search_auctions_page(
                &mut session,
                &mut player,
                &house,
                &StubSeller::named(99, "Boris"),
                &crate::web_pages::default_item_type_manager(),
                SearchRequest {
                    action: Some("bid"),
                    auction_select: Some("5"),
                    bid_amount: amount,
                    ..SearchRequest::default()
                },
            )
            .unwrap();
            assert!(matches!(outcome, SearchOutcome::Redirect(_)), "{amount:?}");
            assert_eq!(
                take_errors(&mut session),
                [BID_AMOUNT_INVALID_MESSAGE],
                "{amount:?}"
            );
            assert!(player.take_bids().is_empty(), "{amount:?}");
        }
    }

    #[test]
    fn bid_action_without_a_selection_errors_but_still_bids() {
        // SearchAuctions.php:495-499: the invalid-selection error has
        // no return - the bid is still attempted with auction id 0.
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                action: Some("bid"),
                auction_select: Some(""),
                bid_amount: Some("50"),
                ..SearchRequest::default()
            },
        )
        .unwrap();

        assert!(matches!(outcome, SearchOutcome::Redirect(_)));
        assert_eq!(take_errors(&mut session), [AUCTION_SELECT_INVALID_MESSAGE]);
        assert_eq!(
            player.take_bids(),
            [BidSubmission {
                auction_id: 0,
                is_buyout: false,
                bid_amount: 50,
            }]
        );
    }

    #[test]
    fn buyout_action_bids_zero_with_the_flag() {
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                action: Some("buyout"),
                auction_select: Some("7"),
                ..SearchRequest::default()
            },
        )
        .unwrap();

        assert!(matches!(outcome, SearchOutcome::Redirect(_)));
        assert_eq!(
            player.take_bids(),
            [BidSubmission {
                auction_id: 7,
                is_buyout: true,
                bid_amount: 0,
            }]
        );
    }

    #[test]
    fn buyout_action_refusal_reports_and_redirects() {
        let mut player = StubPlayer::failing_bid();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                action: Some("buyout"),
                auction_select: Some("7"),
                ..SearchRequest::default()
            },
        )
        .unwrap();

        assert!(matches!(outcome, SearchOutcome::Redirect(_)));
        assert_eq!(take_errors(&mut session), ["bid refused"]);
    }

    #[test]
    fn a_failed_bid_still_redirects_to_the_search_params() {
        // SearchAuctions.php:486-516: the bid leg's
        // addExceptionMsgAndDisconnect redirect is overwritten by the
        // unconditional search-params redirect.
        let mut player = StubPlayer::failing_bid();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                action: Some("bid"),
                auction_select: Some("5"),
                bid_amount: Some("100"),
                ..SearchRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SearchOutcome::Redirect(
                "SearchAuctions.php?search_type_name=&search_min_bid=&search_max_bid=&search_submit="
                    .to_string()
            )
        );
        assert_eq!(take_errors(&mut session), ["bid refused"]);
    }

    #[test]
    fn unknown_actions_fall_through_to_the_search() {
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                action: Some("search"),
                ..SearchRequest::default()
            },
        )
        .unwrap();

        assert!(matches!(outcome, SearchOutcome::Render(_)));
        assert_eq!(
            house.take_calls(),
            [
                "create_bidder(7)",
                "search(Some(Criteria(5)))",
                "info([])",
                "time"
            ]
        );
    }

    #[test]
    fn the_action_redirect_urlencodes_the_search_params() {
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                action: Some("bid"),
                auction_select: Some("5"),
                bid_amount: Some("100"),
                search_type_name: Some("Staff of"),
                search_min_bid: Some("10"),
                search_max_bid: None,
                search_submit: true,
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SearchOutcome::Redirect(
                "SearchAuctions.php?search_type_name=Staff+of&search_min_bid=10&search_max_bid=&search_submit="
                    .to_string()
            )
        );
    }

    #[test]
    fn an_empty_search_submit_retrieves_all() {
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                search_submit: true,
                ..SearchRequest::default()
            },
        )
        .unwrap();

        // No typed criteria: the raw submit retrieves everything.
        assert_eq!(
            house.take_calls(),
            ["create_bidder(7)", "search(None)", "info([])", "time"]
        );
        let SearchOutcome::Render(render) = outcome else {
            panic!("expected a render");
        };
        assert!(render.results.is_empty());
        assert!(render.bids.is_empty());
        assert!(render.no_results_message);
    }

    #[test]
    fn the_default_view_narrows_to_the_players_own_bids() {
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest::default(),
        )
        .unwrap();

        assert_eq!(
            house.take_calls(),
            [
                "create_bidder(7)",
                "search(Some(Criteria(5)))",
                "info([])",
                "time"
            ]
        );
        let SearchOutcome::Render(render) = outcome else {
            panic!("expected a render");
        };
        // Not a submit: no "no auctions" message even when empty.
        assert!(!render.no_results_message);
    }

    #[test]
    fn typed_criteria_combine_in_reference_order() {
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                search_type_name: Some("sword"),
                search_min_bid: Some("10"),
                search_max_bid: Some("50"),
                ..SearchRequest::default()
            },
        )
        .unwrap();

        // eregi over the catalog first, then the bid range, then the
        // bidder set, AND-ed pairwise and OR-ed with the bidder last.
        assert_eq!(
            house.take_calls(),
            [
                "create_item_type([7, 9])",
                "create_bid_range(10, 50)",
                "combine_and(1, 2)",
                "create_bidder(7)",
                "combine_or(5, 3)",
                "search(Some(Criteria(6)))",
                "info([])",
                "time",
            ]
        );
    }

    #[test]
    fn the_bid_range_alone_becomes_the_criteria() {
        let mut player = StubPlayer::default();
        let house = StubHouse::default();
        let mut session = WebSession::new();

        search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                search_min_bid: Some("10"),
                ..SearchRequest::default()
            },
        )
        .unwrap();

        assert_eq!(
            house.take_calls(),
            [
                "create_bid_range(10, -1)",
                "create_bidder(7)",
                "combine_or(5, 2)",
                "search(Some(Criteria(6)))",
                "info([])",
                "time",
            ]
        );
    }

    #[test]
    fn the_type_name_matches_the_catalog_case_insensitively() {
        for (name, expected) in [
            ("STAFF", "[2, 3]"),
            ("sword", "[7, 9]"),
            ("goblet", "[17]"),
            ("axe", "[]"),
        ] {
            let mut player = StubPlayer::default();
            let house = StubHouse::default();
            let mut session = WebSession::new();
            search_auctions_page(
                &mut session,
                &mut player,
                &house,
                &StubSeller::named(99, "Boris"),
                &crate::web_pages::default_item_type_manager(),
                SearchRequest {
                    search_type_name: Some(name),
                    ..SearchRequest::default()
                },
            )
            .unwrap();
            assert_eq!(
                house.take_calls()[0],
                format!("create_item_type({expected})"),
                "{name}"
            );
        }
    }

    #[test]
    fn the_type_criteria_leg_disconnects() {
        // SearchAuctions.php:162-167: the only disconnecting leg inside
        // the search.
        let mut player = StubPlayer::default();
        let house = StubHouse {
            item_type_criteria: Err("criteria refused".to_string()),
            ..StubHouse::default()
        };
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                search_type_name: Some("sword"),
                ..SearchRequest::default()
            },
        );

        assert_eq!(outcome, Err(DISCONNECT_PAGE_URL.to_string()));
        assert_eq!(take_errors(&mut session), ["criteria refused"]);
    }

    #[test]
    fn setup_legs_disconnect() {
        for (leg, mut player, house) in [
            (
                "player info",
                StubPlayer::failing_player(),
                StubHouse::default(),
            ),
            ("gold", StubPlayer::failing_gold(), StubHouse::default()),
        ] {
            let mut session = WebSession::new();
            let outcome = search_auctions_page(
                &mut session,
                &mut player as &mut dyn WebEntityQueries,
                &house,
                &StubSeller::named(99, "Boris"),
                &crate::web_pages::default_item_type_manager(),
                SearchRequest::default(),
            );
            assert_eq!(outcome, Err(DISCONNECT_PAGE_URL.to_string()), "{leg}");
            assert_eq!(take_errors(&mut session).len(), 1, "{leg}");
        }
    }

    #[test]
    fn later_search_legs_report_and_render_empty() {
        let manager = crate::web_pages::default_item_type_manager();
        let typed = SearchRequest {
            search_type_name: Some("sword"),
            search_min_bid: Some("10"),
            ..SearchRequest::default()
        };
        let scenarios: [(&str, StubHouse, SearchRequest); 6] = [
            (
                "combine_and",
                StubHouse {
                    combine_and: Err("and refused".to_string()),
                    ..StubHouse::default()
                },
                typed,
            ),
            (
                "bid range",
                StubHouse {
                    bid_range_criteria: Err("range refused".to_string()),
                    ..StubHouse::default()
                },
                typed,
            ),
            (
                "bidder",
                StubHouse {
                    bidder_criteria: Err("bidder refused".to_string()),
                    ..StubHouse::default()
                },
                SearchRequest::default(),
            ),
            (
                "combine_or",
                StubHouse {
                    combine_or: Err("or refused".to_string()),
                    ..StubHouse::default()
                },
                // Only typed criteria route through combine_or.
                SearchRequest {
                    search_type_name: Some("sword"),
                    ..SearchRequest::default()
                },
            ),
            (
                "search",
                StubHouse {
                    searched: Err("search blew up".to_string()),
                    ..StubHouse::default()
                },
                SearchRequest::default(),
            ),
            (
                "info",
                StubHouse {
                    infos: Err("info blew up".to_string()),
                    ..StubHouse::default()
                },
                SearchRequest::default(),
            ),
        ];
        for (leg, house, request) in scenarios {
            let mut player = StubPlayer::default();
            let mut session = WebSession::new();
            let outcome = search_auctions_page(
                &mut session,
                &mut player,
                &house,
                &StubSeller::named(99, "Boris"),
                &manager,
                request,
            )
            .unwrap();
            let SearchOutcome::Render(render) = outcome else {
                panic!("{leg}: expected a render");
            };
            assert!(render.results.is_empty(), "{leg}");
            assert!(render.bids.is_empty(), "{leg}");
            let errors = take_errors(&mut session);
            assert_eq!(errors.len(), 1, "{leg}");
        }
    }

    #[test]
    fn bought_out_and_expired_auctions_drop_out() {
        let mut bought_out = auction(1);
        bought_out.current_bid = 500;
        let mut expired = auction(2);
        expired.expiry = 999.0;
        let mut own_sale = auction(3);
        own_sale.seller_dbid = PLAYER_DBID;
        let mut mine = auction(4);
        mine.bidders = vec![PLAYER_DBID];
        mine.highest_bidder = Some(PLAYER_DBID);
        mine.current_max_bid = 120;
        let mut too_dear = auction(5);
        too_dear.current_bid = 300;
        let house = StubHouse::with_infos(vec![bought_out, expired, own_sale, mine, too_dear]);
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest {
                search_submit: true,
                ..SearchRequest::default()
            },
        )
        .unwrap();

        // The player's own sale (3) drops entirely; 4 is their bid, 5
        // stays open. One clock call per non-bought-out candidate plus
        // the render stamp.
        let SearchOutcome::Render(render) = outcome else {
            panic!("expected a render");
        };
        assert_eq!(
            render
                .results
                .iter()
                .map(|row| row.auction_id)
                .collect::<Vec<_>>(),
            [5]
        );
        assert_eq!(
            render
                .bids
                .iter()
                .map(|row| row.auction_id)
                .collect::<Vec<_>>(),
            [4]
        );
        assert_eq!(
            render.bids[0].status,
            BidStatus::HighestBidder { max_bid: 120 }
        );
        assert_eq!(render.bids[0].seller_name, "Boris");
        assert_eq!(render.bids[0].expiry_line, "8 mins 20 secs");
        assert_eq!(render.results[0].seller_name, "Boris");
        assert!(!render.results[0].affordable);
        assert_eq!(render.results[0].buyout, Some(500));
        assert_eq!(house.time_calls.get(), 5);
        assert!(!render.no_results_message);
    }

    #[test]
    fn an_outbid_bid_row_says_so() {
        let mut mine = auction(4);
        mine.bidders = vec![PLAYER_DBID, 8];
        mine.highest_bidder = Some(8);
        let house = StubHouse::with_infos(vec![mine]);
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest::default(),
        )
        .unwrap();

        let SearchOutcome::Render(render) = outcome else {
            panic!("expected a render");
        };
        assert_eq!(render.bids[0].status, BidStatus::Outbid);
        // Bid 50 against gold 250: affordable.
        assert!(render.results.is_empty());
    }

    #[test]
    fn the_in_loop_clock_failure_keeps_the_previous_stamp() {
        // webGetTime runs inside the filter loop; a failure there keeps
        // the previous stamp (0.0) while the render still times with
        // the post-search clock.
        let house = StubHouse {
            time_failures: Cell::new(1),
            ..StubHouse::with_infos(vec![auction(1)])
        };
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest::default(),
        )
        .unwrap();

        let SearchOutcome::Render(render) = outcome else {
            panic!("expected a render");
        };
        // Kept (1500 survives the 0.0 stamp); the render still times
        // against the post-search clock (1000), so 500 remains.
        assert_eq!(render.results[0].expiry_line, "8 mins 20 secs");
    }

    #[test]
    fn seller_names_are_cached_per_dbid() {
        let house = StubHouse::with_infos(vec![auction(1), auction(2)]);
        let seller = StubSeller::named(99, "Boris");
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();

        search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &seller,
            &crate::web_pages::default_item_type_manager(),
            SearchRequest::default(),
        )
        .unwrap();

        assert_eq!(*seller.lookups.borrow(), [99]);
    }

    #[test]
    fn a_seller_lookup_failure_keeps_everything_unsplit() {
        // SearchAuctions.php:334-352: the lookup failure returns before
        // the split - bid rows stay in the results, unresolved sellers
        // render blank.
        let mut mine = auction(1);
        mine.bidders = vec![PLAYER_DBID];
        let mut theirs = auction(2);
        theirs.seller_dbid = 55;
        let house = StubHouse::with_infos(vec![mine, theirs]);
        let mut player = StubPlayer::default();
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::failing(55),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest::default(),
        )
        .unwrap();

        let SearchOutcome::Render(render) = outcome else {
            panic!("expected a render");
        };
        assert_eq!(render.bids, Vec::new());
        assert_eq!(
            render
                .results
                .iter()
                .map(|row| (row.auction_id, row.seller_name.as_str()))
                .collect::<Vec<_>>(),
            [(1, "Boris"), (2, "")]
        );
        assert_eq!(take_errors(&mut session), ["avatar gone"]);
    }

    #[test]
    fn affordability_follows_available_gold() {
        let house = StubHouse::with_infos(vec![auction(1)]);
        // Bid 50 against exactly 50 gold: affordable.
        let mut player = StubPlayer {
            inventory: Ok(gold(50)),
            ..StubPlayer::default()
        };
        let mut session = WebSession::new();

        let outcome = search_auctions_page(
            &mut session,
            &mut player,
            &house,
            &StubSeller::named(99, "Boris"),
            &crate::web_pages::default_item_type_manager(),
            SearchRequest::default(),
        )
        .unwrap();

        let SearchOutcome::Render(render) = outcome else {
            panic!("expected a render");
        };
        assert!(render.results[0].affordable);
    }
}
