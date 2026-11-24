//! BW-14-03-04: the sample web front end's auction workflow
//! (`programming/fantasydemo/web/php/`):
//!
//! - `PlayerAuctions.php`: the seller's pending auctions -
//!   `webCreateSellerCriteria` + `webSearchAuctions` +
//!   `webGetAuctionInfo`, the expiry column through
//!   [`time_float_to_string`], and the `buyoutPrice == FALSE ? "-"`
//!   cell. The three `initialise` calls and the criteria creation
//!   catch with `addExceptionMsgAndDisconnect`; the search catch only
//!   adds the message (the auctions variable stays unset, so the page
//!   renders the "no auctions" paragraph). The bare
//!   `webGetAuctionInfo` call has no try/catch in the reference: an
//!   uncaught exception aborts the request, which the port surfaces as
//!   the `Err` leg of [`run_player_auctions_page`].
//! - `SearchAuctions.php`: the criteria algebra
//!   (`webCreateItemTypeCriteria` / `webCreateBidRangeCriteria` /
//!   `webCreateBidderCriteria` combined with `webCombineAnd` /
//!   `webCombineOr`), the bid/buyout actions, and the
//!   results/player-bids split. `webGetGoldAndInventory` sits outside
//!   the reference's try (`SearchAuctions.php:246-248`) and the
//!   per-auction `webGetTime` inside the filter loop is uncaught
//!   (`SearchAuctions.php:335-343`) - both surface as `Err` from
//!   [`run_search_auctions_page`].
//!
//! The `Inventory.php` `action:auction` half of the workflow lives in
//! [`crate::web_character::run_inventory_page`]; this module holds the
//! two auction-house pages.
//!
//! The criteria themselves are opaque - the pages only pass the
//! handles back - so they arrive as [`AuctionCriteria`] handles from
//! the [`AuctionHouseEntity`] seam. The seller name resolution
//! constructs `RemoteEntity("entities_by_id/Avatar/<dbID>")` per
//! seller (`SearchAuctions.php:348-370`); the port takes a
//! [`SellerLookup`] seam instead.

use std::collections::BTreeMap;

use regex::RegexBuilder;

use crate::web_character::{
    EntityError, PageState, PlayerEntity, add_error_msg, add_exception_msg, item_type, item_types,
    persist_redirect, php_int_cast, pull_page_messages,
};
use crate::web_session::WebSession;

/// The criteria handle the auction house entity hands out. The pages
/// only pass them back, so the port keeps them opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuctionCriteria(pub u64);

/// One `webGetAuctionInfo` record (`SearchAuctions.php:313-319`,
/// `PlayerAuctions.php:100-110`).
#[derive(Debug, Clone, PartialEq)]
pub struct AuctionInfo {
    pub auction_id: i64,
    pub item_lock: i64,
    pub item_type: u32,
    pub item_serial: i64,
    pub seller_db_id: i64,
    pub current_bid: i64,
    /// The top bid so far - shown only on the player's own bids.
    pub current_max_bid: i64,
    /// `None` where the reference's array carries null.
    pub buyout_price: Option<i64>,
    pub expiry: f64,
    pub highest_bidder: Option<i64>,
    pub bidders: Vec<i64>,
}

/// The auction house entity face (`RemoteEntity("global_entities/AuctionHouse")`).
pub trait AuctionHouseEntity {
    /// `webCreateSellerCriteria(array("dbID" => ...))`
    /// (`PlayerAuctions.php:71-76`).
    fn web_create_seller_criteria(&mut self, db_id: i64) -> Result<AuctionCriteria, EntityError>;
    /// `webCreateItemTypeCriteria(array("itemTypes" => ...))`
    /// (`SearchAuctions.php:94-101`).
    fn web_create_item_type_criteria(
        &mut self,
        item_types: &[u32],
    ) -> Result<AuctionCriteria, EntityError>;
    /// `webCreateBidRangeCriteria(array("minBid" => ..., "maxBid" => ...))`
    /// (`SearchAuctions.php:137-142`).
    fn web_create_bid_range_criteria(
        &mut self,
        min_bid: i64,
        max_bid: i64,
    ) -> Result<AuctionCriteria, EntityError>;
    /// `webCreateBidderCriteria(array("dbID" => ...))`
    /// (`SearchAuctions.php:186-191`).
    fn web_create_bidder_criteria(&mut self, db_id: i64) -> Result<AuctionCriteria, EntityError>;
    /// `webCombineAnd(array("criteria1" => ..., "criteria2" => ...))`.
    fn web_combine_and(
        &mut self,
        criteria1: AuctionCriteria,
        criteria2: AuctionCriteria,
    ) -> Result<AuctionCriteria, EntityError>;
    /// `webCombineOr(array("criteria1" => ..., "criteria2" => ...))`.
    fn web_combine_or(
        &mut self,
        criteria1: AuctionCriteria,
        criteria2: AuctionCriteria,
    ) -> Result<AuctionCriteria, EntityError>;
    /// `webSearchAuctions(array("criteria" => ...))` - `None` is the
    /// reference's empty criteria, which retrieves all auctions.
    fn web_search_auctions(
        &mut self,
        criteria: Option<&AuctionCriteria>,
    ) -> Result<Vec<i64>, EntityError>;
    /// `webGetAuctionInfo(array("auctions" => ...))`.
    fn web_get_auction_info(&mut self, auctions: &[i64]) -> Result<Vec<AuctionInfo>, EntityError>;
    /// `webGetTime()`, answered with the `time` float.
    fn web_get_time(&mut self) -> Result<f64, EntityError>;
}

/// The seller entity face: `RemoteEntity("entities_by_id/Avatar/<dbID>")`
/// answered with `webGetPlayerInfo`'s `playerName`
/// (`SearchAuctions.php:348-370`).
pub trait SellerLookup {
    fn player_name(&mut self, db_id: i64) -> Result<String, EntityError>;
}

/// `Util.php` `timeFloatToString` at its default granularity
/// (`Util.php:29-101`): the `60/60/24/7` unit chain, plural on
/// `mod > 1`, higher units prepend only when their remainder is
/// non-zero, and the walk stops when a division lands on zero. (A full
/// week renders `"0 secs"` - there is no week unit; the reference
/// behaves the same. The `$granularity` parameter is never passed by
/// the pages and is not ported.)
pub fn time_float_to_string(time: f64) -> String {
    const UNITS: [f64; 4] = [60.0, 60.0, 24.0, 7.0];
    const NAMES: [&str; 4] = ["sec", "min", "hr", "day"];

    let mut out = String::new();
    let negative = time < 0.0;
    let mut time = time.abs();
    for (value, name) in UNITS.iter().zip(NAMES.iter()) {
        let divided = (time / value).trunc();
        let rem = (time - divided * value) as i64;
        let mut unit = format!("{rem} {name}");
        if rem > 1 {
            unit.push('s');
        }
        if !out.is_empty() {
            if rem != 0 {
                out = format!("{unit} {out}");
            }
        } else {
            out = unit;
        }
        time = divided;
        if divided == 0.0 {
            break;
        }
    }
    if negative { format!("-{out}") } else { out }
}

/// PHP's `urlencode`: alphanumerics and `-_.` pass, spaces become `+`,
/// everything else is `%XX` upper-case hex.
pub fn php_urlencode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `(int) min($currentBid + 1, 1.05 * $currentBid)`
/// (`SearchAuctions.php:400`): the float minimum truncates, so bids
/// under 20 recommend themselves (`min(19 + 1, 19.95)` casts 19.95 to
/// 19).
pub fn recommended_bid(current_bid: i64) -> i64 {
    let current_bid = current_bid as f64;
    (current_bid + 1.0).min(current_bid * 1.05) as i64
}

/// PHP's loose `int == $maybe_null` behind the auction comparisons
/// (`currentBid == buyoutPrice`, `highestBidder == playerDBID`): null
/// equals only zero.
fn php_loose_eq_i64(value: i64, other: Option<i64>) -> bool {
    match other {
        Some(other) => value == other,
        None => value == 0,
    }
}

/// The `addExceptionMsgAndDisconnect` redirect target. Both action
/// handlers land here on a caught throw, and both immediately
/// redirect back to the search - the Disconnect.php URL is set and
/// then overwritten by the last `setRedirect` call.
pub const DISCONNECT_PAGE_URL: &str = "Disconnect.php";

/// One rendered `My Auctions` row (`PlayerAuctions.php:143-165`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerAuctionRow {
    pub item_name: String,
    pub item_icon: &'static str,
    pub expiry_left: String,
    pub current_bid: i64,
    /// `buyoutPrice == FALSE ? "-" : ...` - null and 0 both render the
    /// dash (PHP's loose comparison with FALSE).
    pub buyout_cell: String,
}

/// The PlayerAuctions page outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerAuctionsOutcome {
    DisconnectRedirect {
        state: PageState,
    },
    Rendered {
        has_auctions: bool,
        rows: Vec<PlayerAuctionRow>,
        state: PageState,
    },
}

/// The PlayerAuctions page request (`PlayerAuctions.php:39-110`). The
/// `Err` leg is the reference's bare `webGetAuctionInfo` call
/// (`PlayerAuctions.php:100-102`) - no try/catch, the exception
/// aborts the request.
pub fn run_player_auctions_page(
    session: &mut WebSession,
    player: &mut dyn PlayerEntity,
    house: &mut dyn AuctionHouseEntity,
) -> Result<PlayerAuctionsOutcome, EntityError> {
    let mut state = pull_page_messages(session);

    // initialise: the three setup calls share one try, any throw
    // disconnects (`PlayerAuctions.php:46-63`).
    let db_id = match player.web_get_player_info() {
        Ok(info) => info.database_id,
        Err(err) => {
            return Ok(PlayerAuctionsOutcome::DisconnectRedirect {
                state: disconnect(state, session, &err),
            });
        }
    };
    if let Err(err) = player.web_get_gold_and_inventory() {
        return Ok(PlayerAuctionsOutcome::DisconnectRedirect {
            state: disconnect(state, session, &err),
        });
    }
    let official_time = match house.web_get_time() {
        Ok(time) => time,
        Err(err) => {
            return Ok(PlayerAuctionsOutcome::DisconnectRedirect {
                state: disconnect(state, session, &err),
            });
        }
    };

    // doGetPlayerAuctions: the criteria creation disconnects on a
    // throw (`PlayerAuctions.php:70-79`); the search only adds the
    // message and returns - the auctions variable stays unset
    // (`PlayerAuctions.php:81-98`); the info call is bare
    // (`PlayerAuctions.php:100-102`).
    let criteria = match house.web_create_seller_criteria(db_id) {
        Ok(criteria) => criteria,
        Err(err) => {
            return Ok(PlayerAuctionsOutcome::DisconnectRedirect {
                state: disconnect(state, session, &err),
            });
        }
    };
    let auction_ids = match house.web_search_auctions(Some(&criteria)) {
        Ok(ids) => ids,
        Err(err) => {
            add_exception_msg(&mut state, session, &err.0, None);
            return Ok(PlayerAuctionsOutcome::Rendered {
                has_auctions: false,
                rows: Vec::new(),
                state,
            });
        }
    };
    let infos = house.web_get_auction_info(&auction_ids)?;

    let rows = infos
        .iter()
        .map(|info| PlayerAuctionRow {
            item_name: item_type(info.item_type)
                .map_or("", |info| info.name)
                .to_string(),
            item_icon: item_type(info.item_type).map_or("", |info| info.icon_img_url),
            expiry_left: time_float_to_string(info.expiry - official_time),
            current_bid: info.current_bid,
            buyout_cell: if info.buyout_price.is_none_or(|buyout| buyout == 0) {
                "-".to_string()
            } else {
                info.buyout_price.unwrap_or(0).to_string()
            },
        })
        .collect();
    let has_auctions = !infos.is_empty();
    Ok(PlayerAuctionsOutcome::Rendered {
        has_auctions,
        rows,
        state,
    })
}

/// The SearchAuctions page's `$_REQUEST` keys. `action` carries the
/// matched `action:(.+)` submit-button key - `ereg`, case-sensitive,
/// and the last matching key in the request wins
/// (`SearchAuctions.php:251-258`). `search_submit` keeps the raw
/// button value because the action redirect re-encodes it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SearchParams {
    pub search_type_name: Option<String>,
    pub search_min_bid: Option<String>,
    pub search_max_bid: Option<String>,
    pub search_submit: Option<String>,
    pub bid_amount: Option<String>,
    pub auction_select: Option<String>,
    pub action: Option<String>,
}

/// One rendered search-results row (`SearchAuctions.php:380-451`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultRow {
    pub auction_id: i64,
    pub item_name: String,
    pub item_icon: &'static str,
    pub seller_name: String,
    pub expiry_left: String,
    pub current_bid: i64,
    /// `auctionBad` when the current bid is past this player's gold.
    pub bid_class: &'static str,
    /// The radio disables at the same threshold.
    pub radio_disabled: bool,
    pub recommended_bid: i64,
    /// `-` where the reference's array carries null.
    pub buyout_cell: String,
}

/// The My Bids status cell (`SearchAuctions.php:554-573`). The blank
/// arm of the reference's render is unreachable through the
/// classification - a bid row exists only when the player is among the
/// bidders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BidStatus {
    HighestBidder { current_max_bid: i64 },
    Outbid,
}

/// One rendered My Bids row (`SearchAuctions.php:500-583`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerBidRow {
    pub auction_id: i64,
    pub item_name: String,
    pub item_icon: &'static str,
    pub seller_name: String,
    pub expiry_left: String,
    pub current_bid: i64,
    pub recommended_bid: i64,
    /// The buyout price, or `&nbsp;` where null.
    pub buyout_cell: String,
    pub status: BidStatus,
}

/// The SearchAuctions page outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchOutcome {
    DisconnectRedirect {
        state: PageState,
    },
    /// A bid/buyout action (or any `action:` key) redirects back to
    /// the same search with the original parameters
    /// (`SearchAuctions.php:259-308`).
    ActionRedirect {
        url: String,
        state: PageState,
    },
    Rendered {
        /// Empty when nothing matched: the reference renders the "No
        /// auctions found to match your criteria." paragraph only when
        /// the search form was submitted, and no table at all
        /// otherwise.
        search_rows: Vec<SearchResultRow>,
        search_was_submitted: bool,
        bids: Vec<PlayerBidRow>,
        available_gold: i64,
        state: PageState,
    },
}

/// The SearchAuctions page request (`SearchAuctions.php:232-331`).
pub fn run_search_auctions_page(
    session: &mut WebSession,
    params: &SearchParams,
    player: &mut dyn PlayerEntity,
    house: &mut dyn AuctionHouseEntity,
    sellers: &mut dyn SellerLookup,
) -> Result<SearchOutcome, EntityError> {
    let mut state = pull_page_messages(session);

    // initialise (`SearchAuctions.php:243-261`): the player info catch
    // disconnects; the gold read sits outside the try in the
    // reference - an uncaught throw aborts the request.
    let db_id = match player.web_get_player_info() {
        Ok(info) => info.database_id,
        Err(err) => {
            return Ok(SearchOutcome::DisconnectRedirect {
                state: disconnect(state, session, &err),
            });
        }
    };
    let available_gold = player
        .web_get_gold_and_inventory()
        .map(|gold| gold.gold_pieces)?;

    // The action dispatch (`SearchAuctions.php:262-308`): any action
    // key redirects back to the same search, whatever it did.
    let mut redirect: Option<String> = None;
    if let Some(action) = params.action.as_deref() {
        // `urlencode($_REQUEST[$searchParam])` for all four, missing
        // keys urlencoding null to the empty string
        // (`SearchAuctions.php:265-283`).
        let search_params = [
            (
                "search_type_name",
                params.search_type_name.as_deref().unwrap_or(""),
            ),
            (
                "search_min_bid",
                params.search_min_bid.as_deref().unwrap_or(""),
            ),
            (
                "search_max_bid",
                params.search_max_bid.as_deref().unwrap_or(""),
            ),
            (
                "search_submit",
                params.search_submit.as_deref().unwrap_or(""),
            ),
        ];
        let mut url = String::from(SEARCH_AUCTIONS_PAGE_URL);
        url.push('?');
        for (index, (key, value)) in search_params.iter().enumerate() {
            if index > 0 {
                url.push('&');
            }
            url.push_str(key);
            url.push('=');
            url.push_str(&php_urlencode(value));
        }

        if action == "bid" || action == "buyout" {
            // Falsy selection - unset, empty or "0"
            // (`SearchAuctions.php:287-290`).
            let selection = params.auction_select.as_deref();
            let auction_id = selection.map(php_int_cast).unwrap_or(0);
            if selection.is_none() || selection == Some("") || selection == Some("0") {
                add_error_msg(&mut state, "Selected auction is invalid.");
            }
            if action == "bid" {
                // doBidAction (`SearchAuctions.php:40-62`): the amount
                // reads as `(int)` of the raw request value - missing
                // is 0 - and is checked before the entity call. The
                // catch is `addExceptionMsgAndDisconnect`, whose
                // Disconnect.php redirect the search redirect below
                // overwrites.
                let bid_amount = params.bid_amount.as_deref().map(php_int_cast).unwrap_or(0);
                if bid_amount <= 0 {
                    add_error_msg(&mut state, "Bid amount is invalid.");
                } else {
                    match player.web_bid_on_auction(auction_id, false, bid_amount) {
                        Err(err) => add_exception_msg(&mut state, session, &err.0, None),
                        Ok(status) => state.status_msgs.push(status),
                    }
                }
            } else {
                // doBuyoutAction (`SearchAuctions.php:64-83`): no
                // amount, the buyout flag rides instead; its catch is
                // a plain `addExceptionMsg`.
                match player.web_bid_on_auction(auction_id, true, 0) {
                    Err(err) => add_exception_msg(&mut state, session, &err.0, None),
                    Ok(status) => state.status_msgs.push(status),
                }
            }
        }
        redirect = Some(url);
    }
    if let Some(url) = redirect {
        persist_redirect(&state, session);
        return Ok(SearchOutcome::ActionRedirect { url, state });
    }

    // doSearchAction (`SearchAuctions.php:85-404`).
    let mut criteria: Option<AuctionCriteria> = None;

    // The item type criteria: `eregi($searchTypeName, name)` over the
    // whole registry; an invalid pattern matches nothing (the
    // reference's POSIX warning) (`SearchAuctions.php:100-133`).
    if let Some(type_name) = params
        .search_type_name
        .as_deref()
        .filter(|name| !name.is_empty())
    {
        let pattern = RegexBuilder::new(type_name)
            .case_insensitive(true)
            .build()
            .ok();
        let matched: Vec<u32> = item_types()
            .iter()
            .filter(|info| {
                pattern
                    .as_ref()
                    .is_some_and(|pattern| pattern.is_match(info.name))
            })
            .map(|info| info.id)
            .collect();
        let type_criteria = match house.web_create_item_type_criteria(&matched) {
            Ok(criteria) => criteria,
            // The create catch is `addExceptionMsgAndDisconnect`.
            Err(err) => {
                return Ok(SearchOutcome::DisconnectRedirect {
                    state: disconnect(state, session, &err),
                });
            }
        };
        // The type criterion is always the first built, so the
        // reference's `else webCombineAnd` arm here never fires.
        criteria = Some(type_criteria);
    }

    // The bid range: an absent field or an empty one is the -1
    // wildcard (`SearchAuctions.php:135-177`).
    let min_bid = params.search_min_bid.as_deref().map_or(-1, |text| {
        if text.is_empty() {
            -1
        } else {
            php_int_cast(text)
        }
    });
    let max_bid = params.search_max_bid.as_deref().map_or(-1, |text| {
        if text.is_empty() {
            -1
        } else {
            php_int_cast(text)
        }
    });
    if min_bid != -1 || max_bid != -1 {
        let bid_criteria = match house.web_create_bid_range_criteria(min_bid, max_bid) {
            Ok(criteria) => criteria,
            Err(err) => {
                add_exception_msg(&mut state, session, &err.0, None);
                return Ok(render_without_search(state, params, available_gold));
            }
        };
        criteria = Some(match criteria {
            None => bid_criteria,
            Some(existing) => match house.web_combine_and(existing, bid_criteria) {
                Ok(combined) => combined,
                Err(err) => {
                    add_exception_msg(&mut state, session, &err.0, None);
                    return Ok(render_without_search(state, params, available_gold));
                }
            },
        });
    }

    // The bidder criteria always builds; an empty search either
    // retrieves all (the submit button was hit) or narrows to the
    // player's own bids (`SearchAuctions.php:179-224`).
    let bidder_criteria = match house.web_create_bidder_criteria(db_id) {
        Ok(criteria) => criteria,
        Err(err) => {
            add_exception_msg(&mut state, session, &err.0, None);
            return Ok(render_without_search(state, params, available_gold));
        }
    };
    criteria = match criteria {
        None => {
            if params.search_submit.is_some() {
                // Retrieve all: the criteria stay empty.
                None
            } else {
                Some(bidder_criteria)
            }
        }
        // Bidder first: `webCombineOr(criteria1: bidder, criteria2: search)`.
        Some(existing) => match house.web_combine_or(bidder_criteria, existing) {
            Ok(combined) => Some(combined),
            Err(err) => {
                add_exception_msg(&mut state, session, &err.0, None);
                return Ok(render_without_search(state, params, available_gold));
            }
        },
    };

    let auction_ids = match house.web_search_auctions(criteria.as_ref()) {
        Ok(ids) => ids,
        Err(err) => {
            add_exception_msg(&mut state, session, &err.0, None);
            return Ok(render_without_search(state, params, available_gold));
        }
    };
    let infos = match house.web_get_auction_info(&auction_ids) {
        Ok(infos) => infos,
        Err(err) => {
            add_exception_msg(&mut state, session, &err.0, None);
            return Ok(render_without_search(state, params, available_gold));
        }
    };

    // The filter loop: bought-out and expired auctions drop. The
    // `webGetTime` call runs per auction, bare, and stamps the page's
    // official time as it goes (`SearchAuctions.php:331-347`).
    let mut results: Vec<(AuctionInfo, String)> = Vec::new();
    let mut official_time: Option<f64> = None;
    for info in infos {
        if php_loose_eq_i64(info.current_bid, info.buyout_price) {
            continue;
        }
        let now = house.web_get_time()?;
        official_time = Some(now);
        if info.expiry < now {
            continue;
        }
        results.push((info, String::new()));
    }

    // The seller names, cached per seller DBID, then the
    // results/player-bids split (`SearchAuctions.php:349-404`).
    let mut seller_names: BTreeMap<i64, String> = BTreeMap::new();
    let mut search_kept: Vec<(AuctionInfo, String)> = Vec::new();
    let mut bids_kept: Vec<(AuctionInfo, String)> = Vec::new();
    let mut index = 0;
    while index < results.len() {
        let info = results[index].0.clone();
        let seller_name = match seller_names.get(&info.seller_db_id) {
            Some(name) => name.clone(),
            None => match sellers.player_name(info.seller_db_id) {
                Ok(name) => {
                    seller_names.insert(info.seller_db_id, name.clone());
                    name
                }
                Err(err) => {
                    // The reference returns mid-loop: the failing
                    // entry and everything after it stay in the
                    // results, unnamed. The page variables it did set
                    // stay set, so the rows render - against the
                    // official time the filter loop stamped.
                    add_exception_msg(&mut state, session, &err.0, None);
                    for (info, _) in results.drain(index..) {
                        search_kept.push((info, String::new()));
                    }
                    let search_rows = search_kept
                        .iter()
                        .map(|(info, name)| {
                            search_result_row(info, name, official_time, available_gold)
                        })
                        .collect();
                    let bids = bids_kept
                        .iter()
                        .map(|(info, name)| player_bid_row(info, name, db_id, official_time))
                        .collect();
                    return Ok(SearchOutcome::Rendered {
                        search_rows,
                        search_was_submitted: params.search_submit.is_some(),
                        bids,
                        available_gold,
                        state,
                    });
                }
            },
        };

        if info.bidders.contains(&db_id) {
            bids_kept.push((info, seller_name));
        } else if db_id == info.seller_db_id {
            // The player's own auctions drop from the results entirely.
        } else {
            search_kept.push((info, seller_name));
        }
        index += 1;
    }

    // The official time lands again after the search, its throw
    // disconnecting (`SearchAuctions.php:310-318`).
    let official_time = match house.web_get_time() {
        Ok(time) => time,
        Err(err) => {
            return Ok(SearchOutcome::DisconnectRedirect {
                state: disconnect(state, session, &err),
            });
        }
    };
    let search_rows = search_kept
        .iter()
        .map(|(info, name)| search_result_row(info, name, Some(official_time), available_gold))
        .collect();
    let bids = bids_kept
        .iter()
        .map(|(info, name)| player_bid_row(info, name, db_id, Some(official_time)))
        .collect();
    Ok(SearchOutcome::Rendered {
        search_rows,
        search_was_submitted: params.search_submit.is_some(),
        bids,
        available_gold,
        state,
    })
}

/// The render legs that bail out mid-search: the page variables stay
/// unset, so the results are empty.
fn render_without_search(
    state: PageState,
    params: &SearchParams,
    available_gold: i64,
) -> SearchOutcome {
    SearchOutcome::Rendered {
        search_rows: Vec::new(),
        search_was_submitted: params.search_submit.is_some(),
        bids: Vec::new(),
        available_gold,
        state,
    }
}

fn search_result_row(
    info: &AuctionInfo,
    seller_name: &str,
    official_time: Option<f64>,
    available_gold: i64,
) -> SearchResultRow {
    let bad = info.current_bid > available_gold;
    SearchResultRow {
        auction_id: info.auction_id,
        item_name: item_type(info.item_type)
            .map_or("", |info| info.name)
            .to_string(),
        item_icon: item_type(info.item_type).map_or("", |info| info.icon_img_url),
        seller_name: seller_name.to_string(),
        expiry_left: time_float_to_string(info.expiry - official_time.unwrap_or(0.0)),
        current_bid: info.current_bid,
        bid_class: if bad { "auctionBad" } else { "auctionGood" },
        radio_disabled: bad,
        recommended_bid: recommended_bid(info.current_bid),
        buyout_cell: match info.buyout_price {
            Some(buyout) => buyout.to_string(),
            None => "-".to_string(),
        },
    }
}

fn player_bid_row(
    info: &AuctionInfo,
    seller_name: &str,
    db_id: i64,
    official_time: Option<f64>,
) -> PlayerBidRow {
    let status = if php_loose_eq_i64(db_id, info.highest_bidder) {
        BidStatus::HighestBidder {
            current_max_bid: info.current_max_bid,
        }
    } else {
        BidStatus::Outbid
    };
    PlayerBidRow {
        auction_id: info.auction_id,
        item_name: item_type(info.item_type)
            .map_or("", |info| info.name)
            .to_string(),
        item_icon: item_type(info.item_type).map_or("", |info| info.icon_img_url),
        seller_name: seller_name.to_string(),
        expiry_left: time_float_to_string(info.expiry - official_time.unwrap_or(0.0)),
        current_bid: info.current_bid,
        recommended_bid: recommended_bid(info.current_bid),
        buyout_cell: match info.buyout_price {
            Some(buyout) => buyout.to_string(),
            None => "&nbsp;".to_string(),
        },
        status,
    }
}

/// `addExceptionMsgAndDisconnect`'s tail: the composed message, the
/// flash writes, and the Disconnect.php redirect
/// (`AuthenticatedPage.php`).
fn disconnect(mut state: PageState, session: &mut WebSession, err: &EntityError) -> PageState {
    add_exception_msg(&mut state, session, &err.0, None);
    persist_redirect(&state, session);
    state
}

/// The SearchAuctions page URL - the bid/buyout actions redirect back
/// to it with the original search parameters.
pub const SEARCH_AUCTIONS_PAGE_URL: &str = "SearchAuctions.php";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_character::{BW_ITEM_TYPE_STAFF, GoldAndInventory, PlayerInfo};
    use crate::web_session::{SessionToken, WebSession};
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    const ADDR: &str = "10.0.0.7";
    const T0: u64 = 1_000_000;
    const PLAYER_DB_ID: i64 = 1001;
    const OFFICIAL_TIME: f64 = 1000.0;

    fn logged_in_session() -> WebSession {
        let mut session = WebSession::new();
        session.set_session_token(SessionToken {
            ip_address: ADDR.to_string(),
            last_activity: T0,
            username: "player".to_string(),
            entity_type: Some("Avatar".to_string()),
            entity_path: Some("entities_by_id/Avatar/7".to_string()),
        });
        session
    }

    fn player_info() -> PlayerInfo {
        PlayerInfo {
            char_class: "Ranger".to_string(),
            player_name: "Ranger Bob".to_string(),
            health: 87,
            max_health: 100,
            frags: 12,
            database_id: PLAYER_DB_ID,
            position: [1.0, 2.0, 3.0],
            direction: [0.5, 1.0, 1.5],
            online: true,
        }
    }

    fn gold() -> GoldAndInventory {
        GoldAndInventory {
            gold_pieces: 1000,
            inventory_items: Vec::new(),
            locked_items: Vec::new(),
        }
    }

    /// `PlayerAuctions.php:100`'s record shape, `90` seconds from the
    /// fixture official time.
    fn auction_info(id: i64) -> AuctionInfo {
        AuctionInfo {
            auction_id: id,
            item_lock: 40 + id,
            item_type: BW_ITEM_TYPE_STAFF,
            item_serial: 90 + id,
            seller_db_id: 2001,
            current_bid: 15,
            current_max_bid: 20,
            buyout_price: Some(50),
            expiry: OFFICIAL_TIME + 90.0,
            highest_bidder: Some(3001),
            bidders: vec![3001],
        }
    }

    struct StubPlayer {
        info_reply: Result<PlayerInfo, EntityError>,
        gold_reply: Result<GoldAndInventory, EntityError>,
        bid_reply: Result<String, EntityError>,
        bids: RefCell<Vec<(i64, bool, i64)>>,
    }

    impl StubPlayer {
        fn ok() -> Self {
            Self {
                info_reply: Ok(player_info()),
                gold_reply: Ok(gold()),
                bid_reply: Ok("Bid taken.".to_string()),
                bids: RefCell::new(Vec::new()),
            }
        }

        fn failing(message: &str) -> Self {
            let mut stub = Self::ok();
            stub.info_reply = Err(EntityError(message.to_string()));
            stub
        }
    }

    impl PlayerEntity for StubPlayer {
        fn web_get_player_info(&mut self) -> Result<PlayerInfo, EntityError> {
            self.info_reply.clone()
        }

        fn web_get_gold_and_inventory(&mut self) -> Result<GoldAndInventory, EntityError> {
            self.gold_reply.clone()
        }

        fn web_bid_on_auction(
            &mut self,
            auction_id: i64,
            is_buying_out: bool,
            bid_amount: i64,
        ) -> Result<String, EntityError> {
            self.bids
                .borrow_mut()
                .push((auction_id, is_buying_out, bid_amount));
            self.bid_reply.clone()
        }
    }

    #[derive(Default)]
    struct StubHouse {
        time_reply: Option<Result<f64, EntityError>>,
        search_reply: Option<Result<Vec<i64>, EntityError>>,
        info_reply: Option<Result<Vec<AuctionInfo>, EntityError>>,
        seller_criteria_err: Option<String>,
        item_type_criteria_err: Option<String>,
        bid_range_err: Option<String>,
        bidder_criteria_err: Option<String>,
        combine_and_err: Option<String>,
        combine_or_err: Option<String>,
        created_item_types: RefCell<Vec<Vec<u32>>>,
        bid_ranges: RefCell<Vec<(i64, i64)>>,
        search_criteria: RefCell<Vec<Option<u64>>>,
        combines: RefCell<Vec<&'static str>>,
        time_calls: Cell<usize>,
        next_id: Cell<u64>,
    }

    impl StubHouse {
        fn with_infos(infos: Vec<AuctionInfo>) -> Self {
            Self {
                info_reply: Some(Ok(infos)),
                ..Self::default()
            }
        }

        fn criteria(&self) -> AuctionCriteria {
            let id = self.next_id.get() + 1;
            self.next_id.set(id);
            AuctionCriteria(id)
        }

        fn throw_of(err: &Option<String>) -> Result<AuctionCriteria, EntityError> {
            match err {
                Some(message) => Err(EntityError(message.clone())),
                None => unreachable!("no error staged for this criteria call"),
            }
        }
    }

    impl AuctionHouseEntity for StubHouse {
        fn web_create_seller_criteria(
            &mut self,
            _db_id: i64,
        ) -> Result<AuctionCriteria, EntityError> {
            match &self.seller_criteria_err {
                Some(_) => Self::throw_of(&self.seller_criteria_err),
                None => Ok(self.criteria()),
            }
        }

        fn web_create_item_type_criteria(
            &mut self,
            item_types: &[u32],
        ) -> Result<AuctionCriteria, EntityError> {
            self.created_item_types
                .borrow_mut()
                .push(item_types.to_vec());
            match &self.item_type_criteria_err {
                Some(_) => Self::throw_of(&self.item_type_criteria_err),
                None => Ok(self.criteria()),
            }
        }

        fn web_create_bid_range_criteria(
            &mut self,
            min_bid: i64,
            max_bid: i64,
        ) -> Result<AuctionCriteria, EntityError> {
            self.bid_ranges.borrow_mut().push((min_bid, max_bid));
            match &self.bid_range_err {
                Some(_) => Self::throw_of(&self.bid_range_err),
                None => Ok(self.criteria()),
            }
        }

        fn web_create_bidder_criteria(
            &mut self,
            _db_id: i64,
        ) -> Result<AuctionCriteria, EntityError> {
            match &self.bidder_criteria_err {
                Some(_) => Self::throw_of(&self.bidder_criteria_err),
                None => Ok(self.criteria()),
            }
        }

        fn web_combine_and(
            &mut self,
            _criteria1: AuctionCriteria,
            _criteria2: AuctionCriteria,
        ) -> Result<AuctionCriteria, EntityError> {
            self.combines.borrow_mut().push("and");
            match &self.combine_and_err {
                Some(_) => Self::throw_of(&self.combine_and_err),
                None => Ok(self.criteria()),
            }
        }

        fn web_combine_or(
            &mut self,
            _criteria1: AuctionCriteria,
            _criteria2: AuctionCriteria,
        ) -> Result<AuctionCriteria, EntityError> {
            self.combines.borrow_mut().push("or");
            match &self.combine_or_err {
                Some(_) => Self::throw_of(&self.combine_or_err),
                None => Ok(self.criteria()),
            }
        }

        fn web_search_auctions(
            &mut self,
            criteria: Option<&AuctionCriteria>,
        ) -> Result<Vec<i64>, EntityError> {
            self.search_criteria
                .borrow_mut()
                .push(criteria.map(|criteria| criteria.0));
            match &self.search_reply {
                Some(reply) => reply.clone(),
                None => Ok(vec![]),
            }
        }

        fn web_get_auction_info(
            &mut self,
            auctions: &[i64],
        ) -> Result<Vec<AuctionInfo>, EntityError> {
            let _ = auctions;
            match &self.info_reply {
                Some(reply) => reply.clone(),
                None => Ok(vec![]),
            }
        }

        fn web_get_time(&mut self) -> Result<f64, EntityError> {
            self.time_calls.set(self.time_calls.get() + 1);
            match &self.time_reply {
                Some(reply) => reply.clone(),
                None => Ok(OFFICIAL_TIME),
            }
        }
    }

    struct StubSeller {
        names: BTreeMap<i64, String>,
        err_from: Option<i64>,
        calls: RefCell<Vec<i64>>,
    }

    impl StubSeller {
        fn with_names(names: &[(i64, &str)]) -> Self {
            Self {
                names: names
                    .iter()
                    .map(|(id, name)| (*id, name.to_string()))
                    .collect(),
                err_from: None,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn failing_from(id: i64) -> Self {
            let mut stub = Self::with_names(&[(2001, "Seller One")]);
            stub.err_from = Some(id);
            stub
        }
    }

    impl SellerLookup for StubSeller {
        fn player_name(&mut self, db_id: i64) -> Result<String, EntityError> {
            self.calls.borrow_mut().push(db_id);
            if self.err_from == Some(db_id) {
                return Err(EntityError("seller boom".to_string()));
            }
            Ok(self.names.get(&db_id).cloned().unwrap_or_default())
        }
    }

    fn rendered_search_of(
        outcome: SearchOutcome,
    ) -> (
        Vec<SearchResultRow>,
        bool,
        Vec<PlayerBidRow>,
        i64,
        PageState,
    ) {
        match outcome {
            SearchOutcome::Rendered {
                search_rows,
                search_was_submitted,
                bids,
                available_gold,
                state,
            } => (
                search_rows,
                search_was_submitted,
                bids,
                available_gold,
                state,
            ),
            other => panic!("expected a rendered search, got {other:?}"),
        }
    }

    fn search_disconnect_of(outcome: SearchOutcome) -> PageState {
        match outcome {
            SearchOutcome::DisconnectRedirect { state } => state,
            other => panic!("expected a disconnect, got {other:?}"),
        }
    }

    fn search_action_of(outcome: SearchOutcome) -> (String, PageState) {
        match outcome {
            SearchOutcome::ActionRedirect { url, state } => (url, state),
            other => panic!("expected an action redirect, got {other:?}"),
        }
    }

    fn player_auctions_of(
        outcome: PlayerAuctionsOutcome,
    ) -> (bool, Vec<PlayerAuctionRow>, PageState) {
        match outcome {
            PlayerAuctionsOutcome::Rendered {
                has_auctions,
                rows,
                state,
            } => (has_auctions, rows, state),
            other => panic!("expected a rendered page, got {other:?}"),
        }
    }

    fn empty_params() -> SearchParams {
        SearchParams::default()
    }

    #[test]
    fn time_strings_follow_the_util_unit_chain() {
        assert_eq!(time_float_to_string(90.0), "1 min 30 secs");
        assert_eq!(time_float_to_string(45.0), "45 secs");
        assert_eq!(time_float_to_string(59.0), "59 secs");
        // An exact carry renders the leftover unit against the next:
        // `60` walks min first (mod 1), then hr breaks the walk.
        assert_eq!(time_float_to_string(60.0), "1 min 0 sec");
        assert_eq!(time_float_to_string(0.0), "0 sec");
    }

    #[test]
    fn hour_day_and_negative_time_strings() {
        assert_eq!(time_float_to_string(3600.0), "1 hr 0 sec");
        assert_eq!(time_float_to_string(86400.0), "1 day 0 sec");
        assert_eq!(time_float_to_string(360000.0), "4 days 4 hrs 0 sec");
        assert_eq!(time_float_to_string(-90.0), "-1 min 30 secs");
    }

    #[test]
    fn a_full_week_has_no_unit() {
        assert_eq!(time_float_to_string(604800.0), "0 sec");
    }

    #[test]
    fn urlencoding_passes_safe_bytes_and_escapes_the_rest() {
        assert_eq!(php_urlencode("staff of"), "staff+of");
        assert_eq!(php_urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(php_urlencode("Ranger_Bob.1-x"), "Ranger_Bob.1-x");
        assert_eq!(php_urlencode(""), "");
    }

    #[test]
    fn the_recommended_bid_truncates_the_float_min() {
        assert_eq!(recommended_bid(15), 15);
        assert_eq!(recommended_bid(19), 19);
        assert_eq!(recommended_bid(20), 21);
        assert_eq!(recommended_bid(40), 41);
        assert_eq!(recommended_bid(100), 101);
    }

    #[test]
    fn loose_equality_treats_null_as_zero() {
        assert!(php_loose_eq_i64(0, None));
        assert!(!php_loose_eq_i64(5, None));
        assert!(php_loose_eq_i64(5, Some(5)));
        assert!(!php_loose_eq_i64(5, Some(6)));
        assert!(php_loose_eq_i64(0, Some(0)));
    }

    #[test]
    fn player_auctions_lists_rows_with_expiry_and_buyout_dash() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::with_infos(vec![
            auction_info(1),
            AuctionInfo {
                buyout_price: None,
                ..auction_info(2)
            },
            AuctionInfo {
                buyout_price: Some(0),
                ..auction_info(3)
            },
        ]);
        let outcome = run_player_auctions_page(&mut session, &mut player, &mut house).unwrap();
        let (has_auctions, rows, state) = player_auctions_of(outcome);
        assert!(has_auctions);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].item_name, "Staff of Serpents");
        assert_eq!(rows[0].item_icon, "images/items/icon_snake_staff.gif");
        assert_eq!(rows[0].expiry_left, "1 min 30 secs");
        assert_eq!(rows[0].current_bid, 15);
        assert_eq!(rows[0].buyout_cell, "50");
        assert_eq!(rows[1].buyout_cell, "-");
        assert_eq!(rows[2].buyout_cell, "-");
        assert!(state.error_msgs.is_empty());
    }

    #[test]
    fn an_empty_player_auction_search_renders_no_rows() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::default();
        let outcome = run_player_auctions_page(&mut session, &mut player, &mut house).unwrap();
        let (has_auctions, rows, _) = player_auctions_of(outcome);
        assert!(!has_auctions);
        assert!(rows.is_empty());
    }

    #[test]
    fn player_auctions_setup_throws_disconnect() {
        for message in ["info boom", "gold boom"] {
            let mut session = logged_in_session();
            let mut player = StubPlayer::ok();
            if message == "gold boom" {
                player.gold_reply = Err(EntityError(message.to_string()));
            } else {
                player.info_reply = Err(EntityError(message.to_string()));
            }
            let mut house = StubHouse::default();
            let outcome = run_player_auctions_page(&mut session, &mut player, &mut house).unwrap();
            match outcome {
                PlayerAuctionsOutcome::DisconnectRedirect { state } => {
                    assert_eq!(state.error_msgs, vec![message.to_string()]);
                }
                other => panic!("expected a disconnect for {message}, got {other:?}"),
            }
        }
        for stage in ["time", "criteria"] {
            let mut session = logged_in_session();
            let mut player = StubPlayer::ok();
            let mut house = StubHouse::default();
            if stage == "time" {
                house.time_reply = Some(Err(EntityError("time boom".to_string())));
            } else {
                house.seller_criteria_err = Some("criteria boom".to_string());
            }
            let outcome = run_player_auctions_page(&mut session, &mut player, &mut house).unwrap();
            assert!(
                matches!(outcome, PlayerAuctionsOutcome::DisconnectRedirect { .. }),
                "expected a disconnect for {stage}"
            );
        }
    }

    #[test]
    fn a_player_auctions_search_throw_only_adds_the_message() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse {
            search_reply: Some(Err(EntityError("search boom".to_string()))),
            ..StubHouse::default()
        };
        let outcome = run_player_auctions_page(&mut session, &mut player, &mut house).unwrap();
        let (has_auctions, rows, state) = player_auctions_of(outcome);
        assert!(!has_auctions);
        assert!(rows.is_empty());
        assert_eq!(state.error_msgs, vec!["search boom".to_string()]);
        assert_eq!(
            session.take_flash("err_msgs"),
            Some(vec!["search boom".to_string()])
        );
    }

    #[test]
    fn the_bare_auction_info_call_aborts_the_request() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse {
            search_reply: Some(Ok(vec![1])),
            info_reply: Some(Err(EntityError("info boom".to_string()))),
            ..StubHouse::default()
        };
        let err = run_player_auctions_page(&mut session, &mut player, &mut house).unwrap_err();
        assert_eq!(err.0, "info boom");
    }

    #[test]
    fn player_auction_rows_survive_unknown_item_types() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::with_infos(vec![AuctionInfo {
            item_type: 8,
            ..auction_info(1)
        }]);
        let outcome = run_player_auctions_page(&mut session, &mut player, &mut house).unwrap();
        let (_, rows, _) = player_auctions_of(outcome);
        assert_eq!(rows[0].item_name, "");
        assert_eq!(rows[0].item_icon, "");
    }

    #[test]
    fn a_bid_action_redirects_back_with_the_search_parameters() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            search_type_name: Some("Staff of Serpents".to_string()),
            search_submit: Some("Search".to_string()),
            auction_select: Some("5".to_string()),
            bid_amount: Some("10".to_string()),
            action: Some("bid".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (url, _) = search_action_of(outcome);
        assert_eq!(
            url,
            "SearchAuctions.php?search_type_name=Staff+of+Serpents&search_min_bid=&search_max_bid=&search_submit=Search"
        );
        assert_eq!(*player.bids.borrow(), vec![(5, false, 10)]);
        assert!(
            session
                .take_flash("statusMsgs")
                .unwrap()
                .contains(&"Bid taken.".to_string())
        );
    }

    #[test]
    fn an_invalid_bid_amount_stops_before_the_entity_call() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            action: Some("bid".to_string()),
            auction_select: Some("5".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (_, state) = search_action_of(outcome);
        assert_eq!(state.error_msgs, vec!["Bid amount is invalid.".to_string()]);
        assert!(player.bids.borrow().is_empty());
    }

    #[test]
    fn an_invalid_auction_selection_still_bids_with_zero() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            action: Some("bid".to_string()),
            bid_amount: Some("10".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (_, state) = search_action_of(outcome);
        assert_eq!(
            state.error_msgs,
            vec!["Selected auction is invalid.".to_string()]
        );
        assert_eq!(*player.bids.borrow(), vec![(0, false, 10)]);
    }

    #[test]
    fn a_buyout_bids_the_buyout_flag_and_survives_the_throw() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        player.bid_reply = Err(EntityError("buyout boom".to_string()));
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            action: Some("buyout".to_string()),
            auction_select: Some("7".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (_, state) = search_action_of(outcome);
        assert_eq!(state.error_msgs, vec!["buyout boom".to_string()]);
        assert_eq!(*player.bids.borrow(), vec![(7, true, 0)]);
        assert_eq!(
            session.take_flash("err_msgs"),
            Some(vec!["buyout boom".to_string()])
        );
    }

    #[test]
    fn an_unknown_action_still_redirects_without_bidding() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            action: Some("foo".to_string()),
            auction_select: Some("5".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (url, state) = search_action_of(outcome);
        assert_eq!(
            url,
            "SearchAuctions.php?search_type_name=&search_min_bid=&search_max_bid=&search_submit="
        );
        assert!(state.error_msgs.is_empty());
        assert!(player.bids.borrow().is_empty());
    }

    #[test]
    fn a_submitted_search_combines_type_range_and_bidder_criteria() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::with_infos(vec![
            auction_info(1),
            AuctionInfo {
                seller_db_id: 2002,
                ..auction_info(2)
            },
        ]);
        let mut sellers = StubSeller::with_names(&[(2001, "Seller One"), (2002, "Seller Two")]);
        let params = SearchParams {
            search_type_name: Some("staff".to_string()),
            search_min_bid: Some("10".to_string()),
            search_max_bid: Some("40".to_string()),
            search_submit: Some("Search".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (rows, submitted, bids, gold, state) = rendered_search_of(outcome);
        assert_eq!(*house.created_item_types.borrow(), vec![vec![2, 3]]);
        assert_eq!(*house.bid_ranges.borrow(), vec![(10, 40)]);
        assert_eq!(*house.combines.borrow(), vec!["and", "or"]);
        assert!(submitted);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].seller_name, "Seller One");
        assert_eq!(rows[0].expiry_left, "1 min 30 secs");
        assert_eq!(rows[0].recommended_bid, 15);
        assert_eq!(rows[0].bid_class, "auctionGood");
        assert!(!rows[0].radio_disabled);
        assert_eq!(rows[0].buyout_cell, "50");
        assert!(bids.is_empty());
        assert_eq!(gold, 1000);
        assert!(state.error_msgs.is_empty());
        // One lookup per distinct seller.
        assert_eq!(*sellers.calls.borrow(), vec![2001, 2002]);
    }

    #[test]
    fn empty_bid_fields_wildcard_the_range() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            search_min_bid: Some(String::new()),
            search_max_bid: Some(String::new()),
            search_submit: Some("Search".to_string()),
            ..empty_params()
        };
        run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
            .unwrap();
        assert!(house.bid_ranges.borrow().is_empty());
    }

    #[test]
    fn no_criteria_and_no_submit_searches_my_bids() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::with_infos(vec![
            AuctionInfo {
                highest_bidder: Some(PLAYER_DB_ID),
                bidders: vec![PLAYER_DB_ID, 3001],
                ..auction_info(1)
            },
            AuctionInfo {
                highest_bidder: Some(3001),
                bidders: vec![3001, PLAYER_DB_ID],
                buyout_price: None,
                ..auction_info(2)
            },
        ]);
        let mut sellers = StubSeller::with_names(&[(2001, "Seller One")]);
        let outcome = run_search_auctions_page(
            &mut session,
            &empty_params(),
            &mut player,
            &mut house,
            &mut sellers,
        )
        .unwrap();
        let (rows, submitted, bids, _, _) = rendered_search_of(outcome);
        assert_eq!(house.search_criteria.borrow().len(), 1);
        assert!(house.search_criteria.borrow()[0].is_some());
        assert!(rows.is_empty());
        assert!(!submitted);
        assert_eq!(bids.len(), 2);
        assert_eq!(
            bids[0].status,
            BidStatus::HighestBidder {
                current_max_bid: 20
            }
        );
        assert_eq!(bids[1].status, BidStatus::Outbid);
        assert_eq!(bids[1].buyout_cell, "&nbsp;");
        assert_eq!(bids[1].seller_name, "Seller One");
    }

    #[test]
    fn no_criteria_with_submit_retrieves_all_and_drops_own_auctions() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::with_infos(vec![
            AuctionInfo {
                seller_db_id: PLAYER_DB_ID,
                highest_bidder: None,
                bidders: vec![],
                ..auction_info(1)
            },
            auction_info(2),
        ]);
        let mut sellers = StubSeller::with_names(&[(2001, "Seller One")]);
        let params = SearchParams {
            search_submit: Some("Search".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (rows, _, bids, _, _) = rendered_search_of(outcome);
        assert_eq!(*house.search_criteria.borrow(), vec![None]);
        assert_eq!(rows.len(), 1);
        assert!(bids.is_empty());
    }

    #[test]
    fn bought_out_and_expired_auctions_drop() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::with_infos(vec![
            AuctionInfo {
                current_bid: 50,
                ..auction_info(1)
            },
            AuctionInfo {
                expiry: OFFICIAL_TIME - 1.0,
                ..auction_info(2)
            },
            auction_info(3),
        ]);
        let mut sellers = StubSeller::with_names(&[(2001, "Seller One")]);
        let params = SearchParams {
            search_submit: Some("Search".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (rows, _, _, _, _) = rendered_search_of(outcome);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].auction_id, 3);
        // The bought-out check skips its time call; the expired and
        // live auctions each take one, and the official time takes the
        // last.
        assert_eq!(house.time_calls.get(), 3);
    }

    #[test]
    fn a_seller_lookup_failure_keeps_the_remaining_rows_unnamed() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::with_infos(vec![
            // My own bid, resolved and named before the failure.
            AuctionInfo {
                bidders: vec![PLAYER_DB_ID],
                highest_bidder: Some(PLAYER_DB_ID),
                ..auction_info(1)
            },
            auction_info(2),
            AuctionInfo {
                seller_db_id: 2002,
                ..auction_info(3)
            },
        ]);
        let mut sellers = StubSeller::failing_from(2002);
        let params = SearchParams {
            search_submit: Some("Search".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (rows, submitted, bids, _, state) = rendered_search_of(outcome);
        assert_eq!(state.error_msgs, vec!["seller boom".to_string()]);
        // The first auction moved to My Bids, named; the second kept
        // its resolved name, the failing entry and the rest stay
        // unnamed.
        assert_eq!(bids.len(), 1);
        assert_eq!(bids[0].seller_name, "Seller One");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].seller_name, "Seller One");
        assert_eq!(rows[1].seller_name, "");
        assert_eq!(rows[1].expiry_left, "1 min 30 secs");
        assert!(submitted);
    }

    #[test]
    fn mid_search_throws_render_without_results() {
        // The range combine failure bails before the search.
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse {
            combine_and_err: Some("and boom".to_string()),
            ..StubHouse::default()
        };
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            search_type_name: Some("staff".to_string()),
            search_min_bid: Some("10".to_string()),
            search_submit: Some("Search".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let (rows, submitted, bids, _, state) = rendered_search_of(outcome);
        assert_eq!(state.error_msgs, vec!["and boom".to_string()]);
        assert!(rows.is_empty() && bids.is_empty());
        assert!(submitted);
        assert!(house.search_criteria.borrow().is_empty());
    }

    #[test]
    fn an_item_type_criteria_failure_disconnects() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse {
            item_type_criteria_err: Some("type boom".to_string()),
            ..StubHouse::default()
        };
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            search_type_name: Some("staff".to_string()),
            ..empty_params()
        };
        let outcome =
            run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
                .unwrap();
        let state = search_disconnect_of(outcome);
        assert_eq!(state.error_msgs, vec!["type boom".to_string()]);
        assert_eq!(
            session.take_flash("err_msgs"),
            Some(vec!["type boom".to_string()])
        );
    }

    #[test]
    fn other_criteria_failures_only_add_the_message() {
        for stage in ["bid range", "bidder", "or", "search", "info"] {
            let mut session = logged_in_session();
            let mut player = StubPlayer::ok();
            let mut house = StubHouse::default();
            let message = format!("{stage} boom");
            match stage {
                "bid range" => house.bid_range_err = Some(message.clone()),
                "bidder" => house.bidder_criteria_err = Some(message.clone()),
                "or" => house.combine_or_err = Some(message.clone()),
                "search" => house.search_reply = Some(Err(EntityError(message.clone()))),
                _ => house.info_reply = Some(Err(EntityError(message.clone()))),
            }
            let mut sellers = StubSeller::with_names(&[]);
            let needs_type = stage == "or";
            let needs_range = stage == "bid range" || stage == "or";
            let params = SearchParams {
                search_type_name: if needs_type {
                    Some("staff".to_string())
                } else {
                    None
                },
                search_min_bid: if needs_range {
                    Some("10".to_string())
                } else {
                    None
                },
                search_submit: Some("Search".to_string()),
                ..empty_params()
            };
            let outcome = run_search_auctions_page(
                &mut session,
                &params,
                &mut player,
                &mut house,
                &mut sellers,
            )
            .unwrap();
            let (rows, _, bids, _, state) = rendered_search_of(outcome);
            assert_eq!(state.error_msgs, vec![message], "stage {stage}");
            assert!(rows.is_empty() && bids.is_empty(), "stage {stage}");
        }
    }

    #[test]
    fn an_invalid_pattern_matches_no_item_types() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let params = SearchParams {
            search_type_name: Some("[".to_string()),
            search_submit: Some("Search".to_string()),
            ..empty_params()
        };
        run_search_auctions_page(&mut session, &params, &mut player, &mut house, &mut sellers)
            .unwrap();
        assert_eq!(*house.created_item_types.borrow(), vec![vec![]]);
    }

    #[test]
    fn time_throws_disconnect_or_abort_by_position() {
        // The final official-time throw disconnects (the search came
        // back empty, so the filter loop never calls).
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse {
            time_reply: Some(Err(EntityError("late boom".to_string()))),
            ..StubHouse::default()
        };
        let mut sellers = StubSeller::with_names(&[]);
        let outcome = run_search_auctions_page(
            &mut session,
            &empty_params(),
            &mut player,
            &mut house,
            &mut sellers,
        )
        .unwrap();
        search_disconnect_of(outcome);

        // The filter loop's per-auction call is bare: the exception
        // aborts the request.
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        let mut house = StubHouse::with_infos(vec![auction_info(1)]);
        house.time_reply = Some(Err(EntityError("early boom".to_string())));
        let mut sellers = StubSeller::with_names(&[]);
        let err = run_search_auctions_page(
            &mut session,
            &empty_params(),
            &mut player,
            &mut house,
            &mut sellers,
        )
        .unwrap_err();
        assert_eq!(err.0, "early boom");
    }

    #[test]
    fn a_player_info_failure_disconnects_the_search_page() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::failing("who boom");
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let outcome = run_search_auctions_page(
            &mut session,
            &empty_params(),
            &mut player,
            &mut house,
            &mut sellers,
        )
        .unwrap();
        let state = search_disconnect_of(outcome);
        assert_eq!(state.error_msgs, vec!["who boom".to_string()]);
    }

    #[test]
    fn the_gold_read_outside_the_try_aborts_the_request() {
        let mut session = logged_in_session();
        let mut player = StubPlayer::ok();
        player.gold_reply = Err(EntityError("gold boom".to_string()));
        let mut house = StubHouse::default();
        let mut sellers = StubSeller::with_names(&[]);
        let err = run_search_auctions_page(
            &mut session,
            &empty_params(),
            &mut player,
            &mut house,
            &mut sellers,
        )
        .unwrap_err();
        assert_eq!(err.0, "gold boom");
    }
}
