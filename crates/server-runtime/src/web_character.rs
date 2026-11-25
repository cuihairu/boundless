//! BW-14-03-03: the sample web front end's character and inventory
//! pages (`programming/fantasydemo/web/php/`):
//!
//! - `Characters.php`: the character list - logout, the missing-entity
//!   disconnect, the already-logged-in bounce, `webCreateCharacter`,
//!   `webChooseCharacter` and `webGetCharacterList`.
//! - `Character.php`: `webGetPlayerInfo` rendered as the ordered
//!   statistics table, with the rad->deg factor's literal `3.1415` and
//!   the direction tuple read as `(roll, pitch, yaw)`.
//! - `Items.php`: the [`ItemTypeManager`] registry, eight item types
//!   registered verbatim (ids 2..7, 9, 17 - id 8 does not exist).
//! - `Inventory.php`: `webGetGoldAndInventory`, the unlocked-item rows
//!   in serial order and the `view_item` detail pane. The
//!   `action:auction` branch is the BW-14-03-04 workflow
//!   ([`crate::web_auction`]).
//! - `ItemViewer.php` / `ViewItem.php`: the item detail pane and the
//!   legacy standalone view page (whose `$GLOBALS['BW_ITEM_TYPES']`
//!   registry `Items.php` never defines - the reference page therefore
//!   always redirects; the port models the lookup against an injected
//!   registry so both outcomes exist).
//!
//! The pages' semantics are the port, not the XHTML-MP markup: the
//! outcomes carry the redirect URLs, the row data, the composed message
//! strings and the zebra/checked flags the markup layer would render.
//! The `RemoteEntity` calls arrive as the [`AccountEntity`],
//! [`PlayerEntity`] and [`InventoryEntity`] seams - the same device as
//! [`crate::web_session::DbLogOn`]. PHP exceptions carry only a message
//! and `Page::getExceptionMsg` consumes only that message
//! (`Page.php:140-150`), so the seams fail with [`EntityError`].

use std::collections::BTreeMap;
use std::fmt;

use crate::web_session::{WebAuthenticator, WebSession};

/// `Constants.php` `$CHARACTER_WELCOME_PAGE_URL`.
pub const CHARACTER_WELCOME_PAGE_URL: &str = "News.php";
/// `Constants.php` `$ACCOUNT_WELCOME_PAGE_URL`.
pub const ACCOUNT_WELCOME_PAGE_URL: &str = "Characters.php";

/// `Constants.php` `$CHAR_CLASS_NAMES` (name => display name).
pub const CHARACTER_CLASS_NAMES: &[(&str, &str)] = &[
    ("ranger", "Ranger"),
    ("female_warrior", "Female Warrior"),
    ("ped_male", "Civilian"),
    ("barbarian", "Barbarian"),
];

/// `$CHAR_CLASS_NAMES[$class]` - an unknown class renders empty (the
/// reference's undefined-index notice renders as nothing).
pub fn char_class_name(class: &str) -> &'static str {
    CHARACTER_CLASS_NAMES
        .iter()
        .find(|(key, _)| *key == class)
        .map(|(_, name)| *name)
        .unwrap_or("")
}

/// `Constants.php` `BW_ITEM_TYPE_*`. The ids are the reference's, gaps
/// included - id 8 is not defined anywhere.
pub const BW_ITEM_TYPE_STAFF: u32 = 2;
pub const BW_ITEM_TYPE_STAFF_2: u32 = 3;
pub const BW_ITEM_TYPE_DRUMSTICK: u32 = 4;
pub const BW_ITEM_TYPE_SPIDER_LEG: u32 = 5;
pub const BW_ITEM_TYPE_BINOCULARS: u32 = 6;
pub const BW_ITEM_TYPE_SWORD: u32 = 7;
pub const BW_ITEM_TYPE_SWORD_2: u32 = 9;
pub const BW_ITEM_TYPE_GOBLET: u32 = 17;

/// `Character.php:47`: `$radDegFactor = 360.0 / (2. * 3.1415)` - the
/// reference's literal `3.1415`, not `pi()`.
#[allow(clippy::approx_constant)]
pub const RAD_DEG_FACTOR: f64 = 360.0 / (2.0 * 3.1415);

/// An item type in the registry (`Items.php` `ItemType`). The stats
/// keep the reference's insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemType {
    pub id: u32,
    pub name: &'static str,
    pub stats: &'static [(&'static str, &'static str)],
    pub description: &'static str,
    pub icon_img_url: &'static str,
    pub detail_img_url: &'static str,
}

const STAFF_STATS: &[(&str, &str)] = &[("Magic damage", "50"), ("Durability", "100")];
const DRUMSTICK_STATS: &[(&str, &str)] = &[("Health bonus", "+20")];
const SPIDER_LEG_STATS: &[(&str, &str)] = &[("Durability", "100")];
const SWORD_STATS: &[(&str, &str)] = &[
    ("Accuracy", "80%"),
    ("Speed", "3s"),
    ("Power", "40"),
    ("Type", "melee"),
];
const SWORD_2_STATS: &[(&str, &str)] = &[
    ("Accuracy", "75%"),
    ("Speed", "3.5s"),
    ("Power", "60"),
    ("Type", "melee"),
];

/// The `ItemTypeManager` registry in registration order
/// (`Items.php:88-210`).
static ITEM_TYPES: [ItemType; 8] = [
    ItemType {
        id: BW_ITEM_TYPE_STAFF,
        name: "Staff of Serpents",
        stats: STAFF_STATS,
        description: "Staff imbued with the spirit of the snake.",
        icon_img_url: "images/items/icon_snake_staff.gif",
        detail_img_url: "images/items/detail_snake_staff.jpg",
    },
    ItemType {
        id: BW_ITEM_TYPE_STAFF_2,
        name: "Staff of Lightning",
        stats: STAFF_STATS,
        description: "Staff imbued with the electrical essence.",
        icon_img_url: "images/items/icon_lightning_staff.gif",
        detail_img_url: "images/items/detail_lightning_staff.jpg",
    },
    ItemType {
        id: BW_ITEM_TYPE_DRUMSTICK,
        name: "Drumstick",
        stats: DRUMSTICK_STATS,
        description: "Increases health.",
        icon_img_url: "images/items/icon_drumstick.gif",
        detail_img_url: "images/items/detail_drumstick.jpg",
    },
    ItemType {
        id: BW_ITEM_TYPE_SPIDER_LEG,
        name: "Spider leg",
        stats: SPIDER_LEG_STATS,
        description: "A leg from a giant spider.",
        icon_img_url: "images/items/icon_spiderleg.gif",
        detail_img_url: "images/items/detail_spiderleg.jpg",
    },
    ItemType {
        id: BW_ITEM_TYPE_BINOCULARS,
        name: "Binoculars",
        stats: &[],
        description: "Allows you to see into the distance.",
        icon_img_url: "images/items/icon_binoculars1.gif",
        detail_img_url: "images/items/detail_binoculars1.jpg",
    },
    ItemType {
        id: BW_ITEM_TYPE_SWORD,
        name: "Sword",
        stats: SWORD_STATS,
        description: "An elegant weapon for a civilised age. \
                      Perfect for dueling mercenaries.",
        icon_img_url: "images/items/icon_sword2.gif",
        detail_img_url: "images/items/detail_sword2.jpg",
    },
    ItemType {
        id: BW_ITEM_TYPE_SWORD_2,
        name: "Bastard Sword",
        stats: SWORD_2_STATS,
        description: "A (slightly larger) elegant weapon for a civilised \
                      age. Perfect for dueling mercenaries.",
        icon_img_url: "images/items/icon_sword3.gif",
        detail_img_url: "images/items/detail_sword3.jpg",
    },
    ItemType {
        id: BW_ITEM_TYPE_GOBLET,
        name: "Goblet",
        stats: DRUMSTICK_STATS,
        description: "Healing water.",
        icon_img_url: "images/items/icon_goblet2.gif",
        detail_img_url: "images/items/detail_goblet2.jpg",
    },
];

/// The registry in registration order.
pub fn item_types() -> &'static [ItemType] {
    &ITEM_TYPES
}

/// `ItemTypeManager::itemTypes[$id]` - `None` where PHP gets null (an
/// unregistered id, e.g. the missing 8).
pub fn item_type(id: u32) -> Option<&'static ItemType> {
    ITEM_TYPES.iter().find(|item| item.id == id)
}

/// `$_SESSION['display']` - the device capabilities the logon flow
/// records.
#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    pub resolution_width: f64,
    pub resolution_height: f64,
}

/// Characters.php:55-59: the portrait height in the character list -
/// a third of the device height, 100 without a recorded display.
pub fn portrait_height(display: Option<&Display>) -> f64 {
    display
        .map(|display| display.resolution_height / 3.0)
        .unwrap_or(100.0)
}

/// PHP's default `precision = 14` float-to-string conversion (the
/// `%.14G` form): integral values lose the decimal point, fractional
/// ones keep 14 significant digits without trailing zeros.
pub fn php_float_string(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        return format!("{}", value.trunc() as i64);
    }
    let text = format!("{value:e}");
    let exponent: i32 = text
        .split('e')
        .nth(1)
        .and_then(|exponent| exponent.parse().ok())
        .unwrap_or(0);
    let decimals = (13 - exponent).max(0) as usize;
    let mut text = format!("{value:.decimals$}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

/// PHP 8 `is_numeric` for the string forms the request layer can
/// deliver: optional leading/trailing whitespace, optional sign, a
/// decimal mantissa and an optional exponent. `inf` / `nan` / hex are
/// not numeric strings.
pub fn php_is_numeric(text: &str) -> bool {
    let body =
        text.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}'));
    let bytes = body.as_bytes();
    let mut index = 0;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        index += 1;
    }
    let mut digits = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
        digits += 1;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
        index += 1;
        if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
            index += 1;
        }
        let mut exponent_digits = 0;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            exponent_digits += 1;
        }
        if exponent_digits == 0 {
            return false;
        }
    }
    index == bytes.len()
}

/// PHP's `(int)` cast: the leading numeric prefix, whitespace skipped,
/// saturating at the i64 bounds.
pub fn php_int_cast(text: &str) -> i64 {
    let bytes = text.trim_start().as_bytes();
    let mut index = 0;
    let mut sign = 1i128;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        if bytes[index] == b'-' {
            sign = -1;
        }
        index += 1;
    }
    let mut value: i128 = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        value = value * 10 + (bytes[index] - b'0') as i128;
        index += 1;
    }
    (sign * value).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// PHP's array-key normalization: only a canonical decimal integer
/// string becomes an int key (`"5"` -> 5; `"05"`, `"5.0"`, `"abc"`
/// stay string keys and miss int-keyed arrays; an out-of-range digit
/// string would be a float key and misses too).
pub fn php_array_key(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (digits, negative) = if bytes[0] == b'-' {
        (&bytes[1..], true)
    } else {
        (bytes, false)
    };
    if digits.is_empty() || !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return None;
    }
    let mut value: i64 = 0;
    for digit in digits {
        value = value.checked_mul(10)?.checked_add((digit - b'0') as i64)?;
    }
    Some(if negative { -value } else { value })
}

/// The exception text a `RemoteEntity` call throws - the only part the
/// pages consume (`Page.php:140-150`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityError(pub String);

impl fmt::Display for EntityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EntityError {}

/// A `characters` row (`Characters.php:119`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterRecord {
    pub name: String,
    pub char_class: String,
}

/// The `webGetPlayerInfo` reply (`Character.php:32`).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfo {
    pub char_class: String,
    pub player_name: String,
    pub health: i64,
    pub max_health: i64,
    pub frags: i64,
    /// `res["databaseID"]` - the auction pages key on it.
    pub database_id: i64,
    /// `res["position"]` - `(x, y, z)`.
    pub position: [f64; 3],
    /// `res["direction"]` - read as `(roll, pitch, yaw)`
    /// (`Character.php:47`).
    pub direction: [f64; 3],
    pub online: bool,
}

/// An inventory item (`Inventory.php:66-71`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryItem {
    pub serial: i64,
    pub item_type: u32,
    /// A lock in progress: anything but -1 hides the row.
    pub lock_handle: i64,
}

/// The `webGetGoldAndInventory` reply (`Inventory.php:64-70`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldAndInventory {
    pub gold_pieces: i64,
    pub inventory_items: Vec<InventoryItem>,
    pub locked_items: Vec<InventoryItem>,
}

/// The Characters page's `RemoteEntity` face.
pub trait AccountEntity {
    /// `webCreateCharacter(array("name" => ...))` (`Characters.php:55`).
    fn web_create_character(&mut self, name: &str) -> Result<(), EntityError>;
    /// `webChooseCharacter(array("name" => ..., "type" => ...))`,
    /// answered with the chosen entity's type and id
    /// (`Characters.php:66-70`).
    fn web_choose_character(
        &mut self,
        name: &str,
        entity_type: &str,
    ) -> Result<(String, u32), EntityError>;
    /// `webGetCharacterList()`, answered with the `characters` array
    /// (`Characters.php:90-93`).
    fn web_get_character_list(&mut self) -> Result<Vec<CharacterRecord>, EntityError>;
}

/// The Character page's `RemoteEntity` face.
pub trait PlayerEntity {
    /// `webGetPlayerInfo()` (`Character.php:32`).
    fn web_get_player_info(&mut self) -> Result<PlayerInfo, EntityError>;
    /// `webGetGoldAndInventory()` - the auction pages read the gold
    /// off the player entity too (`SearchAuctions.php:248`,
    /// `PlayerAuctions.php:55`).
    fn web_get_gold_and_inventory(&mut self) -> Result<GoldAndInventory, EntityError>;
    /// `webBidOnAuction(array("auctionID" => ..., "isBuyingOut" => ...,
    /// "bidAmount" => ...))` (`SearchAuctions.php:50-77`), answered with
    /// the server's `status` string.
    fn web_bid_on_auction(
        &mut self,
        auction_id: i64,
        is_buying_out: bool,
        bid_amount: i64,
    ) -> Result<String, EntityError>;
}

/// The Inventory page's `RemoteEntity` face (the BW-14-03-04 auction
/// call extends this trait).
pub trait InventoryEntity {
    /// `webGetGoldAndInventory()` (`Inventory.php:64`).
    fn web_get_gold_and_inventory(&mut self) -> Result<GoldAndInventory, EntityError>;
    /// `webCreateAuction(array("itemSerial" => ..., "expiry" => ...,
    /// "startBid" => ..., "buyout" => ...))` (`Inventory.php:115-120`),
    /// answered with the created auction's id.
    fn web_create_auction(
        &mut self,
        item_serial: i64,
        expiry: i64,
        start_bid: i64,
        buyout: i64,
    ) -> Result<AuctionCreated, EntityError>;
}

/// The `webCreateAuction` reply's `auctionID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuctionCreated {
    pub auction_id: i64,
}

/// The page's message arrays (`errorMsgs` / `statusMsgs`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PageState {
    pub error_msgs: Vec<String>,
    pub status_msgs: Vec<String>,
}

/// `AuthenticatedPage::render`'s message setup
/// (`AuthenticatedPage.php:125-149`): the cross-request flash
/// variables move into the page arrays, and the login page's
/// `err_msgs` variable is unregistered on the no-auth-error path.
pub(crate) fn pull_page_messages(session: &mut WebSession) -> PageState {
    let state = PageState {
        error_msgs: session.take_flash("errorMsgs").unwrap_or_default(),
        status_msgs: session.take_flash("statusMsgs").unwrap_or_default(),
    };
    session.take_flash("err_msgs");
    state
}

/// `Page::addErrorMsg` (`Page.php:169-172`): the page array only - no
/// session write.
pub(crate) fn add_error_msg(state: &mut PageState, message: &str) {
    state.error_msgs.push(message.to_string());
}

/// `Page::addExceptionMsg` (`Page.php:152-161`): the composed message
/// joins the page's `errorMsgs` and is appended to the `err_msgs`
/// flash the login page consumes.
pub(crate) fn add_exception_msg(
    state: &mut PageState,
    session: &mut WebSession,
    message: &str,
    additional_info: Option<&str>,
) {
    let composed = match additional_info {
        Some(info) => format!("{info}: {message}"),
        None => message.to_string(),
    };
    state.error_msgs.push(composed.clone());
    session.append_flash("err_msgs", composed);
}

/// `XHTMLMPPage::setRedirect`'s message persistence
/// (`AuthenticatedPage.php:184-190`): non-empty page arrays are
/// written back to the session so the next request renders them.
pub(crate) fn persist_redirect(state: &PageState, session: &mut WebSession) {
    if !state.error_msgs.is_empty() {
        session.set_flash("errorMsgs", state.error_msgs.clone());
    }
    if !state.status_msgs.is_empty() {
        session.set_flash("statusMsgs", state.status_msgs.clone());
    }
}

/// The Characters page request (`Characters.php:28-95`). `params`
/// carries the `$_GET` keys the page reads; `account` is
/// `$this->auth->entity()`.
pub fn run_characters_page(
    session: &mut WebSession,
    params: &CharactersParams,
    account: &mut dyn AccountEntity,
) -> CharactersOutcome {
    let mut state = pull_page_messages(session);
    let mut redirect: Option<String> = None;
    let mut characters = Vec::new();

    let entity_present = WebAuthenticator::entity_path(session).is_some();

    if params.logout {
        redirect = Some("Login.php?logout=1".to_string());
    } else if !entity_present {
        session.append_flash("err_msgs", "Unable to access account".to_string());
        redirect = Some("Disconnect.php".to_string());
    } else if WebAuthenticator::entity_type(session) == Some("Avatar") {
        redirect = Some(CHARACTER_WELCOME_PAGE_URL.to_string());
    } else {
        if let Some(name) = &params.new_character_name {
            if name.is_empty() {
                // The page throws `InvalidFieldError` itself
                // (`Characters.php:50-53`) - no entity call.
                add_exception_msg(
                    &mut state,
                    session,
                    "Character name is empty",
                    Some("Could not create character"),
                );
            } else if let Err(err) = account.web_create_character(name) {
                add_exception_msg(
                    &mut state,
                    session,
                    &err.0,
                    Some("Could not create character"),
                );
            }
            // Outside the try/catch: the success message is added even
            // when the create failed (`Characters.php:60`).
            state
                .status_msgs
                .push("Character creation successful".to_string());
        }

        if let Some(name) = &params.character {
            match account.web_choose_character(name, "Avatar") {
                Err(err) => {
                    add_exception_msg(&mut state, session, &err.0, None);
                    persist_redirect(&state, session);
                    return CharactersOutcome::Redirect {
                        url: "Disconnect.php".to_string(),
                        state,
                    };
                }
                Ok((entity_type, entity_id)) => {
                    state.status_msgs.push("Login successful".to_string());
                    WebAuthenticator::set_entity_details(session, &entity_type, entity_id);
                    redirect = Some("News.php".to_string());
                    // No return: the list is still fetched
                    // (`Characters.php:76-88`).
                }
            }
        }

        match account.web_get_character_list() {
            Err(err) => {
                // Replaces any earlier redirect (`setRedirect`
                // overwrites the URL).
                add_exception_msg(&mut state, session, &err.0, None);
                redirect = Some("Disconnect.php".to_string());
            }
            Ok(records) => characters = records,
        }
    }

    if let Some(url) = redirect {
        persist_redirect(&state, session);
        CharactersOutcome::Redirect { url, state }
    } else {
        CharactersOutcome::Rendered { characters, state }
    }
}

/// The Characters page's `$_GET` keys.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CharactersParams {
    pub logout: bool,
    pub new_character_name: Option<String>,
    pub character: Option<String>,
}

/// The Characters page outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharactersOutcome {
    /// A redirect was set; `state` is what was persisted to the flash.
    Redirect { url: String, state: PageState },
    /// The list renders.
    Rendered {
        characters: Vec<CharacterRecord>,
        state: PageState,
    },
}

/// `Character.php:49-50`: `sprintf("(%.01f, %.01f, %.01f)", x, y, z)`.
pub fn format_position(position: &[f64; 3]) -> String {
    let (x, y, z) = (position[0], position[1], position[2]);
    format!("({x:.1}, {y:.1}, {z:.1})")
}

/// `Character.php:52-60`: the direction tuple is read as
/// `(roll, pitch, yaw)` and printed yaw, pitch, roll, degrees via the
/// literal `3.1415` factor.
pub fn format_direction(direction: &[f64; 3]) -> String {
    let (roll, pitch, yaw) = (direction[0], direction[1], direction[2]);
    format!(
        "(yaw={:.1}, pitch={:.1}, roll={:.1}) [deg]",
        yaw * RAD_DEG_FACTOR,
        pitch * RAD_DEG_FACTOR,
        roll * RAD_DEG_FACTOR
    )
}

/// The Character page request (`Character.php:30-85`).
pub fn run_character_page(
    session: &mut WebSession,
    player: &mut dyn PlayerEntity,
) -> CharacterOutcome {
    let mut state = pull_page_messages(session);

    let info = match player.web_get_player_info() {
        Ok(info) => info,
        Err(err) => {
            add_exception_msg(&mut state, session, &err.0, None);
            persist_redirect(&state, session);
            return CharacterOutcome::DisconnectRedirect { state };
        }
    };

    // The ordered statistics table (`Character.php:67-84`).
    let mut statistics: Vec<(&'static str, String)> = vec![
        ("Class", char_class_name(&info.char_class).to_string()),
        ("Strength", "23".to_string()),
        ("Dexterity", "15".to_string()),
        ("Defense", "21".to_string()),
        ("Intelligence", "18".to_string()),
        (
            "Health",
            format!("{}&nbsp;/&nbsp;{}", info.health, info.max_health),
        ),
        ("Frags", format!("{}", info.frags)),
        (
            "Status",
            if info.online { "online" } else { "offline" }.to_string(),
        ),
    ];
    if info.online {
        statistics.push(("Position", format_position(&info.position)));
        statistics.push(("Direction", format_direction(&info.direction)));
    }

    CharacterOutcome::Rendered {
        title: info.player_name,
        statistics,
        state,
    }
}

/// The Character page outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharacterOutcome {
    DisconnectRedirect {
        state: PageState,
    },
    Rendered {
        title: String,
        statistics: Vec<(&'static str, String)>,
        state: PageState,
    },
}

/// The Inventory page's `$_REQUEST` keys. `action` carries the matched
/// `action:(.*)` submit-button key (`Inventory.php:79-87`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InventoryParams {
    pub view_item: Option<String>,
    pub item_select: Option<String>,
    pub action: Option<String>,
    pub auction_bid_price: Option<String>,
    pub auction_buyout: Option<String>,
    pub auction_expiry: Option<String>,
}

/// A rendered inventory row (`Inventory.php:191-212`): unlocked items
/// in serial order, linked to their registry name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryRow {
    pub serial: i64,
    pub item_type: u32,
    /// `$itemTypeInfo->name` - empty where the registry lookup misses.
    pub item_name: String,
    /// The radio's checked flag: `$_REQUEST['item_select']` equals the
    /// serial (PHP 8's loose numeric comparison).
    pub checked: bool,
    /// `alt<n>` plus `-first` on the first rendered row.
    pub zebra: &'static str,
}

/// The Inventory page's body data (`Inventory.php:154-242`).
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryView {
    pub gold_pieces: i64,
    pub rows: Vec<InventoryRow>,
    /// `rowNum == 0`: the "Inventory is empty." status div replaces the
    /// auction form.
    pub inventory_is_empty: bool,
    /// The create-auction form renders only when rows do.
    pub render_form: bool,
    /// The `view_item` detail pane, when requested.
    pub viewer: Option<ItemViewer>,
}

/// The Inventory page request (`Inventory.php:52-131`, query path).
pub fn run_inventory_page(
    session: &mut WebSession,
    params: &InventoryParams,
    entity: &mut dyn InventoryEntity,
) -> InventoryOutcome {
    let mut state = pull_page_messages(session);

    let inventory = match entity.web_get_gold_and_inventory() {
        Ok(inventory) => inventory,
        Err(err) => {
            add_exception_msg(&mut state, session, &err.0, None);
            persist_redirect(&state, session);
            return InventoryOutcome::DisconnectRedirect { state };
        }
    };

    // itemsOrder / items (`Inventory.php:72-76`).
    let items: BTreeMap<i64, &InventoryItem> = inventory
        .inventory_items
        .iter()
        .map(|item| (item.serial, item))
        .collect();

    // The action dispatch (`Inventory.php:79-131`); the auction case is
    // the BW-14-03-04 workflow. Its early-return paths leave the
    // renderBody data intact - only the created auction redirects.
    let mut redirect: Option<String> = None;
    if params.action.as_deref() == Some("auction") {
        // `!isset($_REQUEST['item_select'])` - unset, not falsy
        // (`Inventory.php:90-95`).
        let mut selected = true;
        if params.item_select.is_none() {
            state
                .status_msgs
                .push("No item selected for auctioning.".to_string());
            selected = false;
        }
        if selected {
            let serial = php_int_cast(params.item_select.as_deref().unwrap_or_default());
            // The bid price: unset or <= 0 rejects
            // (`Inventory.php:99-103`).
            let bid_price = params.auction_bid_price.as_deref().map(php_int_cast);
            if bid_price.is_none_or(|price| price <= 0) {
                add_error_msg(&mut state, "Invalid starting bid price.");
            } else {
                // The buyout: unset or <= 0 becomes the -1 sentinel
                // (`Inventory.php:105-109`).
                let buyout = params
                    .auction_buyout
                    .as_deref()
                    .map(php_int_cast)
                    .filter(|buyout| *buyout > 0)
                    .unwrap_or(-1);
                // The expiry: unset or <= 0 rejects
                // (`Inventory.php:111-116`).
                let expiry = params.auction_expiry.as_deref().map(php_int_cast);
                if expiry.is_none_or(|expiry| expiry <= 0) {
                    add_error_msg(&mut state, "Invalid expiry time.");
                } else {
                    match entity.web_create_auction(
                        serial,
                        expiry.unwrap(),
                        bid_price.unwrap(),
                        buyout,
                    ) {
                        Err(err) => add_exception_msg(&mut state, session, &err.0, None),
                        Ok(created) => {
                            // The item's registry name; an unknown type
                            // renders empty (the reference's null
                            // lookup).
                            let item_name = items
                                .get(&serial)
                                .and_then(|item| item_type(item.item_type))
                                .map(|info| info.name)
                                .unwrap_or("");
                            state
                                .status_msgs
                                .push(format!("Auction of {item_name} created."));
                            redirect = Some(format!(
                                "PlayerAuctions.php?auction_id={}",
                                created.auction_id
                            ));
                        }
                    }
                }
            }
        }
    }

    // The rows: locked items (`lockHandle != -1`) are skipped
    // (`Inventory.php:186-190`).
    let mut row_num = 0usize;
    let mut rows = Vec::new();
    for item in &inventory.inventory_items {
        if item.lock_handle != -1 {
            continue;
        }
        // `alt<n>` with `-first` on the first rendered row
        // (`Inventory.php:211-212`).
        let zebra = if row_num == 0 {
            "alt1-first"
        } else if row_num.is_multiple_of(2) {
            "alt1"
        } else {
            "alt2"
        };
        rows.push(InventoryRow {
            serial: item.serial,
            item_type: item.item_type,
            item_name: item_type(item.item_type)
                .map(|info| info.name)
                .unwrap_or("")
                .to_string(),
            checked: params
                .item_select
                .as_deref()
                .filter(|text| php_is_numeric(text))
                .map(php_int_cast)
                .is_some_and(|value| value == item.serial),
            zebra,
        });
        row_num += 1;
    }

    // The detail pane (`Inventory.php:158-165`): the raw request value
    // canonicalises to an int array key - anything else misses the
    // items map and the viewer keeps its unset fields.
    let viewer = params.view_item.as_ref().map(|text| {
        let serial = php_array_key(text);
        let item_kind = serial
            .and_then(|serial| items.get(&serial))
            .map(|item| item.item_type);
        let mut item_viewer = ItemViewer::new(serial);
        item_viewer.acquire_info(item_kind.and_then(item_type));
        item_viewer
    });

    let inventory_is_empty = rows.is_empty();
    if let Some(url) = redirect {
        persist_redirect(&state, session);
        return InventoryOutcome::Redirect { url, state };
    }
    InventoryOutcome::Rendered {
        view: InventoryView {
            gold_pieces: inventory.gold_pieces,
            rows,
            inventory_is_empty,
            render_form: !inventory_is_empty,
            viewer,
        },
        state,
    }
}

/// The Inventory page outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum InventoryOutcome {
    DisconnectRedirect {
        state: PageState,
    },
    /// A created auction redirects to the player's auction page
    /// (`Inventory.php:123-126`).
    Redirect {
        url: String,
        state: PageState,
    },
    Rendered {
        view: InventoryView,
        state: PageState,
    },
}

/// `ItemViewer` (`ItemViewer.php:11-96`): the item detail pane. The
/// fields start unset (`FALSE` in the reference) and only
/// [`ItemViewer::acquire_info`] fills them - an unknown item type
/// leaves them unset, which is where the reference's null-property
/// notices land.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemViewer {
    /// The raw request value canonicalised to an int array key.
    pub item_id: Option<i64>,
    pub class_name_base: &'static str,
    pub item_type_name: Option<&'static str>,
    pub large_image_url: Option<&'static str>,
    pub description_html: Option<&'static str>,
    pub statistics: Vec<(&'static str, &'static str)>,
}

impl ItemViewer {
    /// `new ItemViewer($itemId)` - the class name base defaults to
    /// `"item-detail"`.
    pub fn new(item_id: Option<i64>) -> Self {
        ItemViewer {
            item_id,
            class_name_base: "item-detail",
            ..ItemViewer::default()
        }
    }

    /// `acquireInfo($itemType)` (`ItemViewer.php:28-56`): fill the
    /// pane from the registry.
    pub fn acquire_info(&mut self, item_type: Option<&'static ItemType>) {
        let Some(item_type) = item_type else {
            return;
        };
        self.item_type_name = Some(item_type.name);
        self.large_image_url = Some(item_type.detail_img_url);
        self.description_html = Some(item_type.description);
        self.statistics = item_type.stats.to_vec();
    }

    /// `output()`'s image URL (`ItemViewer.php:62-75`): scaled through
    /// `Image.php` when the session records a truthy display width,
    /// `0.75 * resolution_width` as a PHP float string.
    pub fn image_url(&self, display: Option<&Display>) -> String {
        match display {
            Some(display) if display.resolution_width != 0.0 => format!(
                "Image.php?path={}&max_width={}",
                self.large_image_url.unwrap_or_default(),
                php_float_string(0.75 * display.resolution_width)
            ),
            _ => self.large_image_url.unwrap_or_default().to_string(),
        }
    }
}

/// `ViewItem.php`'s expected `$GLOBALS['BW_ITEM_TYPES']` record shape.
/// `Items.php` never defines this global, so in the reference runtime
/// the map is absent and every valid id takes the redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyItemType {
    pub name: String,
    pub img_href: String,
    pub description: String,
}

/// The ViewItem page's `$_REQUEST` keys.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ViewItemParams {
    pub item: Option<String>,
    pub referrer: Option<String>,
}

/// The ViewItem page body (`ViewItem.php:71-89`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewItemBody {
    /// `itemId == -1`: "The item you have chosen is not valid."
    NotValid,
    /// The item's thumbnail and description.
    Item {
        name: String,
        img_href: String,
        description: String,
    },
    /// A redirect was set - the body never renders.
    Nothing,
}

/// The ViewItem page outcome (`ViewItem.php:28-58`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewItemPage {
    /// The page title: `"View Item"`, or the item's name once found.
    pub title: String,
    pub redirect: Option<String>,
    pub body: ViewItemBody,
}

/// The ViewItem page request. `bw_item_types` is the page's
/// `$GLOBALS['BW_ITEM_TYPES']` face - pass `None`/an empty map to
/// reproduce the reference runtime, where the global is never defined
/// and every valid id redirects.
pub fn run_view_item_page(
    params: &ViewItemParams,
    bw_item_types: Option<&BTreeMap<i64, LegacyItemType>>,
) -> ViewItemPage {
    let mut page = ViewItemPage {
        title: "View Item".to_string(),
        redirect: None,
        body: ViewItemBody::Nothing,
    };

    // `strlen($itemId) == 0 || !is_numeric($itemId) || (int)$itemId < 0`
    // (`ViewItem.php:32-36`).
    let item_id = params
        .item
        .as_deref()
        .filter(|text| !text.is_empty() && php_is_numeric(text))
        .map(php_int_cast)
        .filter(|value| *value >= 0);

    let Some(item_id) = item_id else {
        page.body = ViewItemBody::NotValid;
        return page;
    };

    match bw_item_types.and_then(|types| types.get(&item_id)) {
        Some(record) => {
            page.title = record.name.clone();
            page.body = ViewItemBody::Item {
                name: record.name.clone(),
                img_href: record.img_href.clone(),
                description: record.description.clone(),
            };
        }
        None => {
            // `setRedirect('ViewItem?referrer=' . $_REQUEST['referrer'])`
            // (`ViewItem.php:43-47`) - the missing `.php` is the
            // reference's.
            page.redirect = Some(format!(
                "ViewItem?referrer={}",
                params.referrer.as_deref().unwrap_or("")
            ));
        }
    }
    page
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const ADDR: &str = "10.0.0.7";
    const T0: u64 = 1_000_000;

    /// A hand-stamped session (the 03-02 login already covers the
    /// stamping path).
    fn logged_in_session(entity_type: &str) -> WebSession {
        let mut session = WebSession::new();
        session.set_session_token(crate::web_session::SessionToken {
            ip_address: ADDR.to_string(),
            last_activity: T0,
            username: "player".to_string(),
            entity_type: Some(entity_type.to_string()),
            entity_path: Some(format!("entities_by_id/{entity_type}/7")),
        });
        session
    }

    struct StubAccount {
        create_reply: Result<(), EntityError>,
        choose_reply: Result<(String, u32), EntityError>,
        list_reply: Result<Vec<CharacterRecord>, EntityError>,
        create_calls: RefCell<usize>,
        choose_calls: RefCell<usize>,
        list_calls: RefCell<usize>,
    }

    impl StubAccount {
        fn ok() -> Self {
            StubAccount {
                create_reply: Ok(()),
                choose_reply: Ok(("Avatar".to_string(), 42)),
                list_reply: Ok(vec![
                    CharacterRecord {
                        name: "Ranger Bob".to_string(),
                        char_class: "ranger".to_string(),
                    },
                    CharacterRecord {
                        name: "崔海如".to_string(),
                        char_class: "barbarian".to_string(),
                    },
                ]),
                create_calls: RefCell::new(0),
                choose_calls: RefCell::new(0),
                list_calls: RefCell::new(0),
            }
        }

        fn failing(message: &str) -> Self {
            StubAccount {
                create_reply: Err(EntityError(message.to_string())),
                choose_reply: Err(EntityError(message.to_string())),
                list_reply: Err(EntityError(message.to_string())),
                create_calls: RefCell::new(0),
                choose_calls: RefCell::new(0),
                list_calls: RefCell::new(0),
            }
        }
    }

    impl AccountEntity for StubAccount {
        fn web_create_character(&mut self, _name: &str) -> Result<(), EntityError> {
            *self.create_calls.borrow_mut() += 1;
            self.create_reply.clone()
        }

        fn web_choose_character(
            &mut self,
            _name: &str,
            _entity_type: &str,
        ) -> Result<(String, u32), EntityError> {
            *self.choose_calls.borrow_mut() += 1;
            self.choose_reply.clone()
        }

        fn web_get_character_list(&mut self) -> Result<Vec<CharacterRecord>, EntityError> {
            *self.list_calls.borrow_mut() += 1;
            self.list_reply.clone()
        }
    }

    fn no_params() -> CharactersParams {
        CharactersParams::default()
    }

    /// Test guards - one line each, so the coverage report carries
    /// four guard lines instead of one per test.
    fn redirect_of(outcome: CharactersOutcome) -> (String, PageState) {
        match outcome {
            CharactersOutcome::Redirect { url, state } => (url, state),
            other => panic!("expected a redirect, got {other:?}"),
        }
    }

    fn rendered_of(outcome: CharactersOutcome) -> (Vec<CharacterRecord>, PageState) {
        match outcome {
            CharactersOutcome::Rendered { characters, state } => (characters, state),
            other => panic!("expected a render, got {other:?}"),
        }
    }

    fn character_page_of(outcome: CharacterOutcome) -> (String, Vec<(&'static str, String)>) {
        match outcome {
            CharacterOutcome::Rendered {
                title, statistics, ..
            } => (title, statistics),
            other => panic!("expected a render, got {other:?}"),
        }
    }

    fn inventory_page_of(outcome: InventoryOutcome) -> (InventoryView, PageState) {
        match outcome {
            InventoryOutcome::Rendered { view, state } => (view, state),
            other => panic!("expected a render, got {other:?}"),
        }
    }

    fn inventory_view_of(outcome: InventoryOutcome) -> InventoryView {
        inventory_page_of(outcome).0
    }

    #[test]
    fn logout_redirects_to_the_login_page() {
        let mut session = logged_in_session("Account");
        let mut account = StubAccount::ok();
        let outcome = run_characters_page(
            &mut session,
            &CharactersParams {
                logout: true,
                ..no_params()
            },
            &mut account,
        );
        assert_eq!(
            outcome,
            CharactersOutcome::Redirect {
                url: "Login.php?logout=1".to_string(),
                state: PageState::default(),
            }
        );
        assert_eq!(*account.list_calls.borrow(), 0, "no entity calls");
    }

    #[test]
    fn a_missing_entity_disconnects_with_the_error_flash() {
        // No entity path: the reference's `!$account` branch.
        let session = WebSession::new();
        let mut session = session;
        let mut account = StubAccount::ok();
        let outcome = run_characters_page(&mut session, &no_params(), &mut account);
        let (url, state) = redirect_of(outcome);
        assert_eq!(url, "Disconnect.php");
        // The message goes straight to the `err_msgs` flash
        // (`$_SESSION['err_msgs'][] = ...`), not the page arrays.
        assert_eq!(state, PageState::default());
        assert_eq!(
            session.take_flash("err_msgs"),
            Some(vec!["Unable to access account".to_string()])
        );
        assert_eq!(session.take_flash("errorMsgs"), None);
    }

    #[test]
    fn an_avatar_session_bounces_to_the_welcome_page() {
        let mut session = logged_in_session("Avatar");
        let mut account = StubAccount::ok();
        let outcome = run_characters_page(&mut session, &no_params(), &mut account);
        assert_eq!(
            outcome,
            CharactersOutcome::Redirect {
                url: CHARACTER_WELCOME_PAGE_URL.to_string(),
                state: PageState::default(),
            }
        );
        assert_eq!(*account.list_calls.borrow(), 0);
    }

    #[test]
    fn an_account_session_renders_the_character_list() {
        let mut session = logged_in_session("Account");
        let mut account = StubAccount::ok();
        let outcome = run_characters_page(&mut session, &no_params(), &mut account);
        let (characters, state) = rendered_of(outcome);
        assert_eq!(characters.len(), 2);
        assert_eq!(characters[0].name, "Ranger Bob");
        assert_eq!(characters[1].char_class, "barbarian");
        assert!(state.error_msgs.is_empty());
        assert_eq!(*account.list_calls.borrow(), 1);
    }

    #[test]
    fn an_empty_name_is_rejected_before_the_entity_is_called() {
        let mut session = logged_in_session("Account");
        let mut account = StubAccount::ok();
        let outcome = run_characters_page(
            &mut session,
            &CharactersParams {
                new_character_name: Some(String::new()),
                ..no_params()
            },
            &mut account,
        );
        let (characters, state) = rendered_of(outcome);
        assert_eq!(*account.create_calls.borrow(), 0, "the page throws first");
        // The composed exception message, then the unconditional
        // success message (`Characters.php:57-60`).
        assert_eq!(
            state.error_msgs,
            vec!["Could not create character: Character name is empty"]
        );
        assert_eq!(state.status_msgs, vec!["Character creation successful"]);
        assert_eq!(characters.len(), 2, "the list still renders");
        assert_eq!(
            session.take_flash("err_msgs"),
            Some(vec![
                "Could not create character: Character name is empty".to_string()
            ])
        );
    }

    #[test]
    fn a_failed_create_still_reports_success() {
        let mut session = logged_in_session("Account");
        // The create fails but the list still answers: the page
        // renders with both messages.
        let mut account = StubAccount::ok();
        account.create_reply = Err(EntityError("The db refused".to_string()));
        let outcome = run_characters_page(
            &mut session,
            &CharactersParams {
                new_character_name: Some("Bob".to_string()),
                ..no_params()
            },
            &mut account,
        );
        let state = rendered_of(outcome).1;
        assert_eq!(
            state.error_msgs,
            vec!["Could not create character: The db refused"]
        );
        assert_eq!(
            state.status_msgs,
            vec!["Character creation successful"],
            "added unconditionally, outside the try/catch"
        );
    }

    #[test]
    fn a_successful_create_adds_only_the_status_message() {
        let mut session = logged_in_session("Account");
        let mut account = StubAccount::ok();
        let outcome = run_characters_page(
            &mut session,
            &CharactersParams {
                new_character_name: Some("Bob".to_string()),
                ..no_params()
            },
            &mut account,
        );
        let state = rendered_of(outcome).1;
        assert_eq!(*account.create_calls.borrow(), 1);
        assert!(state.error_msgs.is_empty());
        assert_eq!(state.status_msgs, vec!["Character creation successful"]);
        // No redirect: the arrays stay on the page and nothing is
        // persisted.
        assert_eq!(session.take_flash("statusMsgs"), None);
    }

    #[test]
    fn choosing_a_character_stamps_the_token_and_redirects() {
        let mut session = logged_in_session("Account");
        let mut account = StubAccount::ok();
        let outcome = run_characters_page(
            &mut session,
            &CharactersParams {
                character: Some("Ranger Bob".to_string()),
                ..no_params()
            },
            &mut account,
        );
        let (url, state) = redirect_of(outcome);
        assert_eq!(*account.choose_calls.borrow(), 1);
        assert_eq!(state.status_msgs, vec!["Login successful"]);
        assert_eq!(url, "News.php");
        // setEntityDetails re-pointed the session at the chosen entity.
        assert_eq!(
            WebAuthenticator::entity_path(&session),
            Some("entities_by_id/Avatar/42")
        );
        // The list is still fetched after the choose - no return
        // (`Characters.php:76-88`).
        assert_eq!(*account.list_calls.borrow(), 1);
        assert_eq!(
            session.take_flash("statusMsgs"),
            Some(vec!["Login successful".to_string()])
        );
    }

    #[test]
    fn a_failed_choose_disconnects_without_a_list_call() {
        let mut session = logged_in_session("Account");
        let mut account = StubAccount::failing("No such character");
        let outcome = run_characters_page(
            &mut session,
            &CharactersParams {
                character: Some("Ghost".to_string()),
                ..no_params()
            },
            &mut account,
        );
        let (url, state) = redirect_of(outcome);
        assert_eq!(url, "Disconnect.php");
        assert_eq!(state.error_msgs, vec!["No such character"]);
        assert_eq!(*account.choose_calls.borrow(), 1);
        assert_eq!(*account.list_calls.borrow(), 0, "the catch returns");
        assert_eq!(
            session.take_flash("errorMsgs"),
            Some(vec!["No such character".to_string()])
        );
    }

    #[test]
    fn a_failed_list_replaces_the_choose_redirect_with_disconnect() {
        // Choose succeeds, then the list throws: the Disconnect
        // redirect overwrites News.php and both messages survive.
        let mut session = logged_in_session("Account");
        let mut account = StubAccount::ok();
        account.list_reply = Err(EntityError("The db vanished".to_string()));
        let outcome = run_characters_page(
            &mut session,
            &CharactersParams {
                character: Some("Ranger Bob".to_string()),
                ..no_params()
            },
            &mut account,
        );
        let (url, state) = redirect_of(outcome);
        assert_eq!(url, "Disconnect.php");
        assert_eq!(
            state.status_msgs,
            vec!["Login successful"],
            "the choose's message survives"
        );
        assert_eq!(state.error_msgs, vec!["The db vanished"]);
    }

    #[test]
    fn a_failing_list_alone_disconnects() {
        let mut session = logged_in_session("Account");
        let mut account = StubAccount::failing("The db vanished");
        let outcome = run_characters_page(&mut session, &no_params(), &mut account);
        assert_eq!(
            outcome,
            CharactersOutcome::Redirect {
                url: "Disconnect.php".to_string(),
                state: PageState {
                    error_msgs: vec!["The db vanished".to_string()],
                    status_msgs: Vec::new(),
                },
            }
        );
    }

    fn player_info() -> PlayerInfo {
        PlayerInfo {
            char_class: "ranger".to_string(),
            player_name: "Ranger Bob".to_string(),
            health: 87,
            max_health: 100,
            frags: 12,
            database_id: 1001,
            position: [1.0, 2.0, 3.0],
            direction: [0.5, 1.0, 1.5],
            online: true,
        }
    }

    struct StubPlayer {
        reply: Result<PlayerInfo, EntityError>,
    }

    impl PlayerEntity for StubPlayer {
        fn web_get_player_info(&mut self) -> Result<PlayerInfo, EntityError> {
            self.reply.clone()
        }

        fn web_get_gold_and_inventory(&mut self) -> Result<GoldAndInventory, EntityError> {
            unreachable!("the character pages never read gold");
        }

        fn web_bid_on_auction(
            &mut self,
            _auction_id: i64,
            _is_buying_out: bool,
            _bid_amount: i64,
        ) -> Result<String, EntityError> {
            unreachable!("the character page never bids");
        }
    }

    #[test]
    fn the_statistics_table_keeps_the_reference_order_online() {
        let mut session = logged_in_session("Avatar");
        let mut player = StubPlayer {
            reply: Ok(player_info()),
        };
        let outcome = run_character_page(&mut session, &mut player);
        let (title, statistics) = character_page_of(outcome);
        assert_eq!(title, "Ranger Bob", "the title becomes the player name");
        let names: Vec<&'static str> = statistics.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                "Class",
                "Strength",
                "Dexterity",
                "Defense",
                "Intelligence",
                "Health",
                "Frags",
                "Status",
                "Position",
                "Direction",
            ]
        );
        let values: Vec<&str> = statistics.iter().map(|(_, value)| value.as_str()).collect();
        assert_eq!(
            values,
            vec![
                "Ranger",
                "23",
                "15",
                "21",
                "18",
                "87&nbsp;/&nbsp;100",
                "12",
                "online",
                "(1.0, 2.0, 3.0)",
                // 1.5 rad through the literal 3.1415 factor.
                "(yaw=85.9, pitch=57.3, roll=28.6) [deg]",
            ]
        );
    }

    #[test]
    fn an_offline_character_has_no_position_or_direction() {
        let mut session = logged_in_session("Avatar");
        let mut info = player_info();
        info.online = false;
        let mut player = StubPlayer { reply: Ok(info) };
        let outcome = run_character_page(&mut session, &mut player);
        let (_, statistics) = character_page_of(outcome);
        let names: Vec<&'static str> = statistics.iter().map(|(name, _)| *name).collect();
        assert_eq!(names.last().copied(), Some("Status"));
        assert_eq!(
            statistics
                .iter()
                .find(|(name, _)| *name == "Status")
                .unwrap()
                .1,
            "offline"
        );
    }

    #[test]
    fn an_unknown_class_renders_an_empty_class_name() {
        // `$CHAR_CLASS_NAMES[$class]` on an unknown key: the notice
        // renders as nothing.
        assert_eq!(char_class_name("no_such_class"), "");
        assert_eq!(char_class_name("ped_male"), "Civilian");
    }

    #[test]
    fn a_failing_player_info_disconnects() {
        let mut session = logged_in_session("Avatar");
        let mut player = StubPlayer {
            reply: Err(EntityError("Not online".to_string())),
        };
        let outcome = run_character_page(&mut session, &mut player);
        assert_eq!(
            outcome,
            CharacterOutcome::DisconnectRedirect {
                state: PageState {
                    error_msgs: vec!["Not online".to_string()],
                    status_msgs: Vec::new(),
                },
            }
        );
        assert_eq!(
            session.take_flash("errorMsgs"),
            Some(vec!["Not online".to_string()])
        );
    }

    #[test]
    fn the_item_registry_carries_the_reference_data_in_order() {
        let types = item_types();
        let ids: Vec<u32> = types.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec![2, 3, 4, 5, 6, 7, 9, 17], "id 8 does not exist");
        assert_eq!(item_type(8), None);

        let staff = item_type(BW_ITEM_TYPE_STAFF).unwrap();
        assert_eq!(staff.name, "Staff of Serpents");
        assert_eq!(
            staff.stats,
            &[("Magic damage", "50"), ("Durability", "100")]
        );
        assert_eq!(
            staff.description,
            "Staff imbued with the spirit of the snake."
        );
        assert_eq!(staff.icon_img_url, "images/items/icon_snake_staff.gif");
        assert_eq!(staff.detail_img_url, "images/items/detail_snake_staff.jpg");

        let sword = item_type(BW_ITEM_TYPE_SWORD).unwrap();
        assert_eq!(
            sword.stats,
            &[
                ("Accuracy", "80%"),
                ("Speed", "3s"),
                ("Power", "40"),
                ("Type", "melee")
            ]
        );
        assert_eq!(
            sword.description,
            "An elegant weapon for a civilised age. Perfect for dueling mercenaries."
        );

        let bastard = item_type(BW_ITEM_TYPE_SWORD_2).unwrap();
        assert_eq!(bastard.name, "Bastard Sword");
        assert_eq!(bastard.stats[1], ("Speed", "3.5s"));

        let binoculars = item_type(BW_ITEM_TYPE_BINOCULARS).unwrap();
        assert!(binoculars.stats.is_empty());

        let goblet = item_type(BW_ITEM_TYPE_GOBLET).unwrap();
        assert_eq!(goblet.name, "Goblet");
        assert_eq!(goblet.stats, &[("Health bonus", "+20")]);
        assert_eq!(goblet.description, "Healing water.");
    }

    struct StubInventory {
        reply: Result<GoldAndInventory, EntityError>,
        create_reply: Result<AuctionCreated, EntityError>,
        create_calls: RefCell<Vec<(i64, i64, i64, i64)>>,
    }

    impl InventoryEntity for StubInventory {
        fn web_get_gold_and_inventory(&mut self) -> Result<GoldAndInventory, EntityError> {
            self.reply.clone()
        }

        fn web_create_auction(
            &mut self,
            item_serial: i64,
            expiry: i64,
            start_bid: i64,
            buyout: i64,
        ) -> Result<AuctionCreated, EntityError> {
            self.create_calls
                .borrow_mut()
                .push((item_serial, expiry, start_bid, buyout));
            self.create_reply.clone()
        }
    }

    fn inventory_reply() -> GoldAndInventory {
        GoldAndInventory {
            gold_pieces: 250,
            inventory_items: vec![
                InventoryItem {
                    serial: 5,
                    item_type: BW_ITEM_TYPE_STAFF,
                    lock_handle: -1,
                },
                InventoryItem {
                    serial: 9,
                    item_type: BW_ITEM_TYPE_DRUMSTICK,
                    lock_handle: 3,
                },
                InventoryItem {
                    serial: 11,
                    item_type: 999,
                    lock_handle: -1,
                },
                InventoryItem {
                    serial: 13,
                    item_type: BW_ITEM_TYPE_SWORD,
                    lock_handle: -1,
                },
            ],
            locked_items: vec![InventoryItem {
                serial: 9,
                item_type: BW_ITEM_TYPE_DRUMSTICK,
                lock_handle: 3,
            }],
        }
    }

    #[test]
    fn the_inventory_rows_skip_locked_items_and_zebra() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let outcome = run_inventory_page(&mut session, &InventoryParams::default(), &mut entity);
        let (view, state) = inventory_page_of(outcome);
        assert_eq!(view.gold_pieces, 250);
        let serials: Vec<i64> = view.rows.iter().map(|row| row.serial).collect();
        assert_eq!(serials, vec![5, 11, 13], "serial 9 is locked");
        let zebras: Vec<&'static str> = view.rows.iter().map(|row| row.zebra).collect();
        assert_eq!(zebras, vec!["alt1-first", "alt2", "alt1"]);
        // Row names resolve through the registry; the unknown item type
        // renders empty (the reference's null lookup).
        assert_eq!(view.rows[0].item_name, "Staff of Serpents");
        assert_eq!(view.rows[1].item_name, "");
        assert_eq!(view.rows[2].item_name, "Sword");
        assert!(!view.inventory_is_empty);
        assert!(view.render_form);
        assert!(view.viewer.is_none(), "no view_item requested");
        assert!(state.error_msgs.is_empty());
    }

    #[test]
    fn the_checked_flag_follows_the_selected_serial() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let outcome = run_inventory_page(
            &mut session,
            &InventoryParams {
                item_select: Some("5".to_string()),
                ..Default::default()
            },
            &mut entity,
        );
        let view = inventory_view_of(outcome);
        assert!(view.rows[0].checked);
        assert!(!view.rows[1].checked);
        assert!(!view.rows[2].checked);
    }

    #[test]
    fn an_all_locked_inventory_renders_the_empty_message_without_the_form() {
        let mut session = logged_in_session("Avatar");
        let mut reply = inventory_reply();
        for item in &mut reply.inventory_items {
            item.lock_handle = 0;
        }
        let mut entity = StubInventory {
            reply: Ok(reply),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let outcome = run_inventory_page(&mut session, &InventoryParams::default(), &mut entity);
        let view = inventory_view_of(outcome);
        assert!(view.rows.is_empty());
        assert!(view.inventory_is_empty);
        assert!(!view.render_form);
        assert_eq!(view.gold_pieces, 250, "the gold row still renders");
    }

    #[test]
    fn a_failing_inventory_query_disconnects() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Err(EntityError("webGetGoldAndInventory failed".to_string())),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let outcome = run_inventory_page(&mut session, &InventoryParams::default(), &mut entity);
        assert_eq!(
            outcome,
            InventoryOutcome::DisconnectRedirect {
                state: PageState {
                    error_msgs: vec!["webGetGoldAndInventory failed".to_string()],
                    status_msgs: Vec::new(),
                },
            }
        );
        assert_eq!(
            session.take_flash("errorMsgs"),
            Some(vec!["webGetGoldAndInventory failed".to_string()])
        );
    }

    #[test]
    fn view_item_opens_the_detail_pane_from_the_registry() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let outcome = run_inventory_page(
            &mut session,
            &InventoryParams {
                view_item: Some("5".to_string()),
                ..Default::default()
            },
            &mut entity,
        );
        let view = inventory_view_of(outcome);
        let viewer = view.viewer.as_ref().unwrap();
        assert_eq!(viewer.item_id, Some(5));
        assert_eq!(viewer.item_type_name, Some("Staff of Serpents"));
        assert_eq!(
            viewer.large_image_url,
            Some("images/items/detail_snake_staff.jpg")
        );
        assert_eq!(
            viewer.statistics,
            vec![("Magic damage", "50"), ("Durability", "100")]
        );
    }

    #[test]
    fn a_view_item_serial_outside_the_inventory_leaves_the_pane_unset() {
        // Serial 999 passes the key check but no item carries it: the
        // reference's `$this->items[999]['itemType']` lands as null and
        // the viewer keeps its FALSE fields.
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let outcome = run_inventory_page(
            &mut session,
            &InventoryParams {
                view_item: Some("999".to_string()),
                ..Default::default()
            },
            &mut entity,
        );
        let view = inventory_view_of(outcome);
        let viewer = view.viewer.as_ref().unwrap();
        assert_eq!(viewer.item_id, Some(999));
        assert_eq!(viewer.item_type_name, None);
        assert_eq!(viewer.large_image_url, None);
        assert_eq!(viewer.description_html, None);
        assert!(viewer.statistics.is_empty());
    }

    #[test]
    fn a_non_canonical_view_item_value_misses_the_items_map() {
        // `"05"` stays a string array key in PHP - the lookup misses.
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let outcome = run_inventory_page(
            &mut session,
            &InventoryParams {
                view_item: Some("05".to_string()),
                ..Default::default()
            },
            &mut entity,
        );
        let view = inventory_view_of(outcome);
        let viewer = view.viewer.as_ref().unwrap();
        assert_eq!(viewer.item_id, None);
        assert_eq!(viewer.item_type_name, None);
    }

    #[test]
    fn the_detail_image_scales_through_image_php_with_a_display() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let outcome = run_inventory_page(
            &mut session,
            &InventoryParams {
                view_item: Some("5".to_string()),
                ..Default::default()
            },
            &mut entity,
        );
        let view = inventory_view_of(outcome);
        let viewer = view.viewer.as_ref().unwrap();

        // 0.75 * 320 = 240 renders without a decimal point.
        let display = Display {
            resolution_width: 320.0,
            resolution_height: 480.0,
        };
        assert_eq!(
            viewer.image_url(Some(&display)),
            "Image.php?path=images/items/detail_snake_staff.jpg&max_width=240"
        );
        // 0.75 * 250 = 187.5 keeps its fraction.
        let display = Display {
            resolution_width: 250.0,
            resolution_height: 480.0,
        };
        assert_eq!(
            viewer.image_url(Some(&display)),
            "Image.php?path=images/items/detail_snake_staff.jpg&max_width=187.5"
        );
        // A zero width is falsy in PHP: the raw URL.
        let display = Display {
            resolution_width: 0.0,
            resolution_height: 480.0,
        };
        assert_eq!(
            viewer.image_url(Some(&display)),
            "images/items/detail_snake_staff.jpg"
        );
        // No display recorded: the raw URL.
        assert_eq!(
            viewer.image_url(None),
            "images/items/detail_snake_staff.jpg"
        );
    }

    #[test]
    fn the_portrait_height_is_a_third_of_the_device_height() {
        assert_eq!(portrait_height(None), 100.0);
        let display = Display {
            resolution_width: 240.0,
            resolution_height: 320.0,
        };
        assert_eq!(portrait_height(Some(&display)), 320.0 / 3.0);
        // And the PHP string form of that height (precision = 14).
        assert_eq!(php_float_string(320.0 / 3.0), "106.66666666667");
    }

    #[test]
    fn php_float_strings_match_the_precision_14_form() {
        assert_eq!(php_float_string(240.0), "240");
        assert_eq!(php_float_string(187.5), "187.5");
        assert_eq!(php_float_string(100.0), "100");
        assert_eq!(php_float_string(0.001), "0.001");
        assert_eq!(php_float_string(-2.5), "-2.5");
        // A value the 14-digit form rounds up to an integer: the trim
        // has to take the bare decimal point too.
        assert_eq!(php_float_string(1.9999999999999998), "2");
    }

    #[test]
    fn php_is_numeric_covers_the_request_forms() {
        for text in [
            "123", "1.5", ".5", "5.", "-3", "+7", "1e3", "1E-2", " 42 ", "+.5", "-1.25e+3",
        ] {
            assert!(php_is_numeric(text), "{text:?} should be numeric");
        }
        for text in [
            "", "abc", "0x1A", "12a", "1.2.3", "inf", "NaN", "1e", "e5", "--3",
        ] {
            assert!(!php_is_numeric(text), "{text:?} should not be numeric");
        }
    }

    #[test]
    fn php_int_cast_takes_the_leading_prefix() {
        assert_eq!(php_int_cast("12"), 12);
        assert_eq!(php_int_cast("12.9"), 12, "the cast truncates");
        assert_eq!(php_int_cast("-3.7"), -3);
        assert_eq!(php_int_cast(" 42"), 42, "leading whitespace skips");
        assert_eq!(php_int_cast("+7"), 7);
        assert_eq!(php_int_cast("abc"), 0, "no digits is zero");
        assert_eq!(php_int_cast("99999999999999999999"), i64::MAX, "saturates");
        assert_eq!(php_int_cast("-99999999999999999999"), i64::MIN);
    }

    #[test]
    fn php_array_keys_canonicalise_only_int_form() {
        assert_eq!(php_array_key("5"), Some(5));
        assert_eq!(php_array_key("-13"), Some(-13));
        assert_eq!(php_array_key("0"), Some(0));
        assert_eq!(php_array_key("-0"), Some(0));
        assert_eq!(php_array_key("05"), None, "leading zero stays a string");
        assert_eq!(php_array_key("5.0"), None);
        assert_eq!(php_array_key("abc"), None);
        assert_eq!(php_array_key(""), None);
        assert_eq!(php_array_key("99999999999999999999"), None, "float key");
    }

    #[test]
    fn entity_errors_display_as_the_raw_message() {
        let err = EntityError("Character name is empty".to_string());
        assert_eq!(err.to_string(), "Character name is empty");
    }

    fn legacy_registry() -> BTreeMap<i64, LegacyItemType> {
        let mut types = BTreeMap::new();
        types.insert(
            2,
            LegacyItemType {
                name: "Staff of Serpents".to_string(),
                img_href: "images/items/icon_snake_staff.gif".to_string(),
                description: "Staff imbued with the spirit of the snake.".to_string(),
            },
        );
        types
    }

    #[test]
    fn a_non_numeric_or_negative_item_param_is_not_valid() {
        for item in [
            None,
            Some(String::new()),
            Some("abc".to_string()),
            Some("-5".to_string()),
        ] {
            let page = run_view_item_page(
                &ViewItemParams {
                    item,
                    referrer: None,
                },
                Some(&legacy_registry()),
            );
            assert_eq!(page.title, "View Item");
            assert_eq!(page.redirect, None);
            assert_eq!(page.body, ViewItemBody::NotValid);
        }
    }

    #[test]
    fn a_valid_id_against_the_reference_runtime_redirects() {
        // `$GLOBALS['BW_ITEM_TYPES']` is never defined: the lookup
        // misses and the page redirects (the reference's `.php`-less
        // URL verbatim).
        let page = run_view_item_page(
            &ViewItemParams {
                item: Some("2".to_string()),
                referrer: Some("Character.php".to_string()),
            },
            Some(&BTreeMap::new()),
        );
        assert_eq!(
            page.redirect.as_deref(),
            Some("ViewItem?referrer=Character.php")
        );
        assert_eq!(page.body, ViewItemBody::Nothing);

        // No referrer at all renders as the empty string.
        let page = run_view_item_page(
            &ViewItemParams {
                item: Some("2".to_string()),
                referrer: None,
            },
            None,
        );
        assert_eq!(page.redirect.as_deref(), Some("ViewItem?referrer="));
    }

    #[test]
    fn a_known_id_renders_the_legacy_record() {
        let page = run_view_item_page(
            &ViewItemParams {
                item: Some("2".to_string()),
                referrer: Some("Character.php".to_string()),
            },
            Some(&legacy_registry()),
        );
        assert_eq!(page.redirect, None);
        assert_eq!(page.title, "Staff of Serpents");
        assert_eq!(
            page.body,
            ViewItemBody::Item {
                name: "Staff of Serpents".to_string(),
                img_href: "images/items/icon_snake_staff.gif".to_string(),
                description: "Staff imbued with the spirit of the snake.".to_string(),
            }
        );
    }

    #[test]
    fn the_pull_drains_the_cross_request_flash_variables() {
        // AuthenticatedPage::render pulls errorMsgs / statusMsgs into
        // the page arrays and unregisters err_msgs before initialise.
        let mut session = logged_in_session("Account");
        session.set_flash("errorMsgs", vec!["earlier error".to_string()]);
        session.set_flash("statusMsgs", vec!["earlier status".to_string()]);
        session.append_flash("err_msgs", "stale login error".to_string());
        let mut account = StubAccount::ok();
        let outcome = run_characters_page(&mut session, &no_params(), &mut account);
        let state = rendered_of(outcome).1;
        assert_eq!(state.error_msgs, vec!["earlier error"]);
        assert_eq!(state.status_msgs, vec!["earlier status"]);
        assert_eq!(session.take_flash("err_msgs"), None, "unregistered");
        assert_eq!(session.take_flash("errorMsgs"), None, "cleared by the pull");
    }

    #[test]
    fn the_radians_factor_is_the_reference_literal() {
        // `360.0 / (2. * 3.1415)` - not `pi()`; one radian in degrees.
        let degrees = RAD_DEG_FACTOR;
        assert!((degrees - 57.297_469_361).abs() < 1e-8, "{degrees}");
        assert_eq!(
            format_direction(&[0.0, 0.0, 1.0]),
            "(yaw=57.3, pitch=0.0, roll=0.0) [deg]"
        );
    }
    #[test]
    fn an_auction_creation_redirects_to_the_new_auction() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let params = InventoryParams {
            action: Some("auction".to_string()),
            item_select: Some("5".to_string()),
            auction_bid_price: Some("5".to_string()),
            auction_buyout: Some("10".to_string()),
            auction_expiry: Some("3600".to_string()),
            ..Default::default()
        };
        let outcome = run_inventory_page(&mut session, &params, &mut entity);
        match outcome {
            InventoryOutcome::Redirect { url, state } => {
                assert_eq!(url, "PlayerAuctions.php?auction_id=42");
                assert_eq!(
                    state.status_msgs,
                    vec!["Auction of Staff of Serpents created.".to_string()]
                );
            }
            other => panic!("expected the auction redirect, got {other:?}"),
        }
        assert_eq!(*entity.create_calls.borrow(), vec![(5, 3600, 5, 10)]);
        // The status rides the redirect into the session.
        assert_eq!(
            session.take_flash("statusMsgs"),
            Some(vec!["Auction of Staff of Serpents created.".to_string()])
        );
    }

    #[test]
    fn an_auction_without_a_selection_only_reports_it() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let params = InventoryParams {
            action: Some("auction".to_string()),
            ..Default::default()
        };
        let outcome = run_inventory_page(&mut session, &params, &mut entity);
        let (_, state) = inventory_page_of(outcome);
        assert_eq!(
            state.status_msgs,
            vec!["No item selected for auctioning.".to_string()]
        );
        assert!(entity.create_calls.borrow().is_empty());
        assert!(session.take_flash("statusMsgs").is_none());
    }

    #[test]
    fn an_auction_without_a_price_reports_it_page_only() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let params = InventoryParams {
            action: Some("auction".to_string()),
            item_select: Some("5".to_string()),
            ..Default::default()
        };
        let outcome = run_inventory_page(&mut session, &params, &mut entity);
        let (_, state) = inventory_page_of(outcome);
        assert_eq!(
            state.error_msgs,
            vec!["Invalid starting bid price.".to_string()]
        );
        assert!(entity.create_calls.borrow().is_empty());
    }

    #[test]
    fn an_auction_without_an_expiry_reports_it() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let params = InventoryParams {
            action: Some("auction".to_string()),
            item_select: Some("5".to_string()),
            auction_bid_price: Some("5".to_string()),
            ..Default::default()
        };
        let outcome = run_inventory_page(&mut session, &params, &mut entity);
        let (_, state) = inventory_page_of(outcome);
        assert_eq!(state.error_msgs, vec!["Invalid expiry time.".to_string()]);
        assert!(entity.create_calls.borrow().is_empty());
    }

    #[test]
    fn an_unset_buyout_rides_the_sentinel() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let params = InventoryParams {
            action: Some("auction".to_string()),
            item_select: Some("5".to_string()),
            auction_bid_price: Some("5".to_string()),
            auction_expiry: Some("60".to_string()),
            ..Default::default()
        };
        let outcome = run_inventory_page(&mut session, &params, &mut entity);
        match outcome {
            InventoryOutcome::Redirect { .. } => {}
            other => panic!("expected the auction redirect, got {other:?}"),
        }
        assert_eq!(*entity.create_calls.borrow(), vec![(5, 60, 5, -1)]);
    }

    #[test]
    fn a_failing_auction_creation_reports_the_exception_twice() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Err(EntityError("create boom".to_string())),
            create_calls: RefCell::new(Vec::new()),
        };
        let params = InventoryParams {
            action: Some("auction".to_string()),
            item_select: Some("5".to_string()),
            auction_bid_price: Some("5".to_string()),
            auction_expiry: Some("60".to_string()),
            ..Default::default()
        };
        let outcome = run_inventory_page(&mut session, &params, &mut entity);
        let (_, state) = inventory_page_of(outcome);
        assert_eq!(state.error_msgs, vec!["create boom".to_string()]);
        assert_eq!(
            session.take_flash("err_msgs"),
            Some(vec!["create boom".to_string()])
        );
        assert!(session.take_flash("statusMsgs").is_none());
    }

    #[test]
    fn an_unknown_selected_item_renders_an_empty_name() {
        let mut session = logged_in_session("Avatar");
        let mut entity = StubInventory {
            reply: Ok(inventory_reply()),
            create_reply: Ok(AuctionCreated { auction_id: 42 }),
            create_calls: RefCell::new(Vec::new()),
        };
        let params = InventoryParams {
            action: Some("auction".to_string()),
            item_select: Some("11".to_string()),
            auction_bid_price: Some("5".to_string()),
            auction_expiry: Some("60".to_string()),
            ..Default::default()
        };
        let outcome = run_inventory_page(&mut session, &params, &mut entity);
        match outcome {
            InventoryOutcome::Redirect { url, .. } => {
                assert_eq!(url, "PlayerAuctions.php?auction_id=42");
            }
            other => panic!("expected the auction redirect, got {other:?}"),
        }
        assert_eq!(
            session.take_flash("statusMsgs"),
            Some(vec!["Auction of  created.".to_string()])
        );
    }
}
