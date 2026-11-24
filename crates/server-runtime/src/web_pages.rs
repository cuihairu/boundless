//! BW-14-03-03: character and item queries.
//!
//! Ported from FantasyDemo's web front end
//! (`programming/fantasydemo/web/php/`) on top of [`crate::web_session`]:
//!
//! - `Items.php`: the [`ItemType`] catalog and its manager, seeded with
//!   the eight sample registrations (ids from `Constants.php`,
//!   `BW_ITEM_TYPE_*`, mirroring `ItemBase.py`).
//! - `Characters.php`: the account page - logout, create
//!   (`webCreateCharacter`), choose (`webChooseCharacter`), and the
//!   character list (`webGetCharacterList`).
//! - `Character.php`: the avatar page - `webGetPlayerInfo` rendered into
//!   the fixed statistics order, with positions in `%.01f` triples and
//!   the direction converted with the reference's own radians factor.
//! - `ViewItem.php`: the id validation (empty / non-numeric / negative is
//!   the -1 sentinel) and the invalid-item paragraph.
//! - `Inventory.php`: `webGetGoldAndInventory` flattened into an ordered
//!   serial list plus a serial-keyed map, locked rows skipped, the
//!   `Inventory is empty.` notice, and the optional item viewer.
//!
//! The pages call the entity's `web*` methods through a `RemoteEntity`;
//! that transport boundary arrives as the [`WebEntityQueries`] trait,
//! the same seam [`crate::web_session::DbLogOn`] uses for `logOn`.

use std::collections::BTreeMap;
use std::fmt;

use crate::web_session::{LOGIN_PAGE_URL, WebAuthenticator, WebSession};

/// `Constants.php` `ACCOUNT_WELCOME_PAGE_URL` - after choosing an avatar
/// the account page sends the player to the character welcome page.
pub const CHARACTER_WELCOME_PAGE_URL: &str = "News.php";
/// The disconnect page every `addExceptionMsgAndDisconnect` lands on.
pub const DISCONNECT_PAGE_URL: &str = "Disconnect.php";
/// The session flash keys (`Page.php:160` writes `err_msgs`; the status
/// messages travel the same way).
pub const FLASH_KEY_ERR_MSGS: &str = "err_msgs";
pub const FLASH_KEY_STATUS_MSGS: &str = "status_msgs";

/// `Constants.php` item type ids, mirroring `ItemBase.py` through the
/// comment block above the definitions.
pub const BW_ITEM_TYPE_STAFF: u32 = 2;
pub const BW_ITEM_TYPE_STAFF_2: u32 = 3;
pub const BW_ITEM_TYPE_DRUMSTICK: u32 = 4;
pub const BW_ITEM_TYPE_SPIDER_LEG: u32 = 5;
pub const BW_ITEM_TYPE_BINOCULARS: u32 = 6;
pub const BW_ITEM_TYPE_SWORD: u32 = 7;
pub const BW_ITEM_TYPE_SWORD_2: u32 = 9;
pub const BW_ITEM_TYPE_GOBLET: u32 = 17;

/// `Character.php:70-75`: the hardcoded base statistics.
pub const CHARACTER_STRENGTH: u32 = 23;
pub const CHARACTER_DEXTERITY: u32 = 15;
pub const CHARACTER_DEFENSE: u32 = 21;
pub const CHARACTER_INTELLIGENCE: u32 = 18;

/// `Character.php:61`: the radians-to-degrees factor, kept at the
/// reference's own imprecise literal (`360.0 / (2. * 3.1415)`), not
/// `PI`.
#[allow(clippy::approx_constant)]
pub const RAD_DEG_FACTOR: f32 = 360.0 / (2.0 * 3.1415);

/// `ViewItem.php`: the invalid-id sentinel.
pub const INVALID_ITEM_ID: i64 = -1;
/// `ViewItem.php:80`: the body paragraph for the sentinel.
pub const INVALID_ITEM_MESSAGE: &str = "The item you have chosen is not valid.";

/// `Constants.php` `$CHAR_CLASS_NAMES`.
pub fn char_class_name(class: &str) -> Option<&'static str> {
    match class {
        "ranger" => Some("Ranger"),
        "female_warrior" => Some("Female Warrior"),
        "ped_male" => Some("Civilian"),
        "barbarian" => Some("Barbarian"),
        _ => None,
    }
}

/// An entity-side failure, carried to the page's
/// `addExceptionMsg` / `addExceptionMsgAndDisconnect` as the exception
/// message (`Page.php:140-150`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebError(pub String);

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The entity-side `web*` method surface the pages call through
/// `RemoteEntity`. The reference calls them on the account entity
/// (`Characters.php`, `Inventory.php`) and on the chosen player entity
/// (`Character.php`).
pub trait WebEntityQueries {
    /// `Account.webCreateCharacter(array("name" => ...))`.
    fn web_create_character(&mut self, name: &str) -> Result<(), WebError>;
    /// `Account.webChooseCharacter(array("name" => ..., "type" =>
    /// "Avatar"))`, answered with the chosen entity's type and id
    /// (`Characters.php:84`).
    fn web_choose_character(&mut self, name: &str) -> Result<(String, u32), WebError>;
    /// `Account.webGetCharacterList()` (`Characters.php:91`).
    fn web_get_character_list(&self) -> Result<Vec<CharacterSummary>, WebError>;
    /// `Avatar.webGetPlayerInfo()` (`Character.php:40`).
    fn web_get_player_info(&self) -> Result<PlayerInfo, WebError>;
    /// `entity.webGetGoldAndInventory()` (`Inventory.php:41`).
    fn web_get_gold_and_inventory(&self) -> Result<GoldAndInventory, WebError>;
    /// `Avatar.webCreateAuction(array("itemSerial" => ..., "expiry" => ...,
    /// "startBid" => ..., "buyout" => ...))`, answered with the auction
    /// id (`Inventory.php:109`) - the auction workflow's submission leg
    /// lives in [`crate::web_auctions`].
    fn web_create_auction(&mut self, request: AuctionSubmission) -> Result<u64, WebError>;
    /// `Avatar.webBidOnAuction(array("auctionID" => ..., "isBuyingOut"
    /// => ..., "bidAmount" => ...))`, answered with a status message
    /// (`SearchAuctions.php:68`, `:106`).
    fn web_bid_on_auction(&mut self, request: BidSubmission) -> Result<String, WebError>;
}

/// The `webCreateAuction` arguments (`Inventory.php:109-113`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuctionSubmission {
    pub item_serial: u64,
    /// Seconds until the auction expires.
    pub expiry: u64,
    pub start_bid: u64,
    /// `-1` when no buyout price was given (`Inventory.php:97`).
    pub buyout: i64,
}

/// The `webBidOnAuction` arguments (`SearchAuctions.php:63-66`, `:103-105`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BidSubmission {
    pub auction_id: u64,
    pub is_buyout: bool,
    pub bid_amount: u64,
}

/// One `webGetCharacterList` entry (`Characters.php:122-137` reads
/// `name` and `charClass`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterSummary {
    pub name: String,
    /// The class key, rendered through [`char_class_name`].
    pub char_class: String,
}

/// The `webGetPlayerInfo` reply (`Character.php:50-59`); the auction
/// pages additionally read `databaseID`
/// (`PlayerAuctions.php:43`, `SearchAuctions.php:449`).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfo {
    pub char_class: String,
    /// The reply's `databaseID`.
    pub database_id: u32,
    pub player_name: String,
    pub health: i32,
    pub max_health: i32,
    pub frags: i32,
    /// The position triple, printed `(x, y, z)`.
    pub position: [f32; 3],
    /// The direction triple in the reply's order (roll, pitch, yaw) -
    /// `Character.php:59` unpacks it that way but prints yaw first.
    pub direction: [f32; 3],
    pub online: bool,
}

/// One `inventoryItems` entry (`Inventory.php:56-60`, `:177`, `:182`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryItem {
    pub serial: u64,
    pub item_type: u32,
    /// `-1` when the item is not locked; locked rows are hidden.
    pub lock_handle: i64,
}

/// The `webGetGoldAndInventory` reply (`Inventory.php:51-53`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldAndInventory {
    pub gold_pieces: u64,
    pub inventory_items: Vec<InventoryItem>,
    /// The reply's `lockedItems` list, carried through unread - the
    /// reference copies it into the page state and never reads it.
    pub locked_items: Vec<u64>,
}

/// `Page.php:152-161` with no `additionalInfo`: the exception message
/// lands in the session flash.
pub(crate) fn add_error_msg(session: &mut WebSession, message: String) {
    let mut messages = session.take_flash(FLASH_KEY_ERR_MSGS).unwrap_or_default();
    messages.push(message);
    session.set_flash(FLASH_KEY_ERR_MSGS, messages);
}

/// `Page.php:174-177`.
pub(crate) fn add_status_msg(session: &mut WebSession, message: String) {
    let mut messages = session
        .take_flash(FLASH_KEY_STATUS_MSGS)
        .unwrap_or_default();
    messages.push(message);
    session.set_flash(FLASH_KEY_STATUS_MSGS, messages);
}

/// `BWAuthenticator::setEntityDetails`: restamp the token's entity
/// details with the chosen entity.
fn set_entity_details(session: &mut WebSession, entity_type: &str, entity_id: u32) {
    if let Some(token) = session.token() {
        let mut token = token.clone();
        token.entity_type = Some(entity_type.to_string());
        token.entity_path = Some(format!("entities_by_id/{entity_type}/{entity_id}"));
        session.set_session_token(token);
    }
}

/// The `Characters.php` request parameters (`$_GET`).
#[derive(Debug, Default, Clone, Copy)]
pub struct CharactersRequest<'a> {
    pub logout: Option<&'a str>,
    pub new_character_name: Option<&'a str>,
    pub character: Option<&'a str>,
}

/// What one `Characters.php` request produced.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CharactersOutcome {
    /// The page redirect, `None` when the page renders the list.
    pub redirect: Option<String>,
    /// The `webGetCharacterList` characters - empty whenever the page
    /// bailed out before the query.
    pub character_list: Vec<CharacterSummary>,
}

/// `CharactersPage::initialise` (`Characters.php:22-100`).
pub fn characters_page(
    session: &mut WebSession,
    account: Option<&mut dyn WebEntityQueries>,
    request: CharactersRequest<'_>,
) -> CharactersOutcome {
    // Characters.php:26-30: any logout argument goes back through the
    // login page.
    if request.logout.is_some() {
        return CharactersOutcome {
            redirect: Some(format!("{LOGIN_PAGE_URL}?logout=1")),
            character_list: Vec::new(),
        };
    }

    // Characters.php:32-37: without the account entity the page
    // disconnects.
    let Some(account) = account else {
        add_error_msg(session, "Unable to access account".to_string());
        return CharactersOutcome {
            redirect: Some(DISCONNECT_PAGE_URL.to_string()),
            character_list: Vec::new(),
        };
    };

    // Characters.php:39-43: an already-chosen avatar is sent to the
    // welcome page.
    if WebAuthenticator::entity_type(session) == Some("Avatar") {
        return CharactersOutcome {
            redirect: Some(CHARACTER_WELCOME_PAGE_URL.to_string()),
            character_list: Vec::new(),
        };
    }

    let mut redirect = None;

    // Characters.php:45-65: create. The status message is added even
    // when the create threw - the catch only records the error and
    // falls through to the unconditional addStatusMsg.
    if let Some(name) = request.new_character_name {
        if name.is_empty() {
            add_error_msg(
                session,
                "Could not create character: Character name is empty".to_string(),
            );
        } else if let Err(err) = account.web_create_character(name) {
            add_error_msg(session, format!("Could not create character: {err}"));
        }
        add_status_msg(session, "Character creation successful".to_string());
    }

    // Characters.php:67-87: choose. A failure disconnects.
    if let Some(name) = request.character {
        match account.web_choose_character(name) {
            Ok((entity_type, entity_id)) => {
                add_status_msg(session, "Login successful".to_string());
                set_entity_details(session, &entity_type, entity_id);
                redirect = Some(CHARACTER_WELCOME_PAGE_URL.to_string());
            }
            Err(err) => {
                add_error_msg(session, err.to_string());
                return CharactersOutcome {
                    redirect: Some(DISCONNECT_PAGE_URL.to_string()),
                    character_list: Vec::new(),
                };
            }
        }
    }

    // Characters.php:89-99: the list is fetched even after a redirect
    // was set.
    match account.web_get_character_list() {
        Ok(characters) => CharactersOutcome {
            redirect,
            character_list: characters,
        },
        Err(err) => {
            add_error_msg(session, err.to_string());
            CharactersOutcome {
                redirect: Some(DISCONNECT_PAGE_URL.to_string()),
                character_list: Vec::new(),
            }
        }
    }
}

/// The `Character.php` render data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterDetails {
    /// The page title: the player's name (`Character.php:68`).
    pub title: String,
    /// The statistics box, name/value pairs in the reference's render
    /// order (`Character.php:70-85`).
    pub statistics: Vec<(&'static str, String)>,
}

/// `CharacterPage::initialise` (`Character.php:33-86`). A failing query
/// disconnects (`Err` carries the redirect).
pub fn character_page(
    session: &mut WebSession,
    player: &dyn WebEntityQueries,
) -> Result<CharacterDetails, String> {
    let info = match player.web_get_player_info() {
        Ok(info) => info,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };

    // Character.php:56-57: positions print as `%.01f` triples.
    let position = format!(
        "({:.1}, {:.1}, {:.1})",
        info.position[0], info.position[1], info.position[2]
    );
    // Character.php:59-66: the direction arrives (roll, pitch, yaw) but
    // prints yaw first, converted with the reference's radians factor.
    let direction = format!(
        "(yaw={:.1}, pitch={:.1}, roll={:.1}) [deg]",
        info.direction[2] * RAD_DEG_FACTOR,
        info.direction[1] * RAD_DEG_FACTOR,
        info.direction[0] * RAD_DEG_FACTOR
    );

    let mut statistics = vec![
        (
            "Class",
            char_class_name(&info.char_class)
                .unwrap_or_default()
                .to_string(),
        ),
        ("Strength", CHARACTER_STRENGTH.to_string()),
        ("Dexterity", CHARACTER_DEXTERITY.to_string()),
        ("Defense", CHARACTER_DEFENSE.to_string()),
        ("Intelligence", CHARACTER_INTELLIGENCE.to_string()),
        (
            "Health",
            format!("{}\u{a0}/\u{a0}{}", info.health, info.max_health),
        ),
        ("Frags", info.frags.to_string()),
        (
            "Status",
            if info.online { "online" } else { "offline" }.to_string(),
        ),
    ];
    // Character.php:81-85: position and direction exist only while
    // online.
    if info.online {
        statistics.push(("Position", position));
        statistics.push(("Direction", direction));
    }

    Ok(CharacterDetails {
        title: info.player_name,
        statistics,
    })
}

/// One catalog entry (`Items.php:9-27`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemType {
    pub id: u32,
    pub name: String,
    /// The stat name/value pairs in the reference's order.
    pub stats: Vec<(String, String)>,
    pub description: String,
    pub icon_img_url: String,
    pub detail_img_url: String,
}

/// `ItemTypeManager` (`Items.php:54-88`).
#[derive(Debug, Default, Clone)]
pub struct ItemTypeManager {
    item_types: BTreeMap<u32, ItemType>,
}

impl ItemTypeManager {
    /// An empty manager (`Items.php:57-60`).
    pub fn new() -> Self {
        Self::default()
    }

    /// `registerItemType` (`Items.php:69-72`): keyed by the item type
    /// id, a later registration replaces an earlier one.
    pub fn register_item_type(&mut self, item_type: ItemType) {
        self.item_types.insert(item_type.id, item_type);
    }

    /// `renderItemType` (`Items.php:74-86`): whether the id is
    /// registered. The reference tests `in_array` over the item
    /// *objects*, which can never match an id - the port checks the
    /// evident intent, the keys.
    pub fn render_item_type(&self, id: u32) -> bool {
        self.item_types.contains_key(&id)
    }

    /// The registered item type, if any.
    pub fn get(&self, id: u32) -> Option<&ItemType> {
        self.item_types.get(&id)
    }

    /// The registered item type ids, ascending
    /// (`SearchAuctions.php:134` iterates them).
    pub fn item_type_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.item_types.keys().copied()
    }
}

/// `Items.php:90-206`: the page-load registrations, verbatim.
pub fn default_item_type_manager() -> ItemTypeManager {
    fn item(
        id: u32,
        name: &str,
        stats: &[(&str, &str)],
        description: &str,
        icon_img_url: &str,
        detail_img_url: &str,
    ) -> ItemType {
        ItemType {
            id,
            name: name.to_string(),
            stats: stats
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            description: description.to_string(),
            icon_img_url: icon_img_url.to_string(),
            detail_img_url: detail_img_url.to_string(),
        }
    }

    let mut manager = ItemTypeManager::new();
    manager.register_item_type(item(
        BW_ITEM_TYPE_STAFF,
        "Staff of Serpents",
        &[("Magic damage", "50"), ("Durability", "100")],
        "Staff imbued with the spirit of the snake.",
        "images/items/icon_snake_staff.gif",
        "images/items/detail_snake_staff.jpg",
    ));
    manager.register_item_type(item(
        BW_ITEM_TYPE_STAFF_2,
        "Staff of Lightning",
        &[("Magic damage", "50"), ("Durability", "100")],
        "Staff imbued with the electrical essence.",
        "images/items/icon_lightning_staff.gif",
        "images/items/detail_lightning_staff.jpg",
    ));
    manager.register_item_type(item(
        BW_ITEM_TYPE_DRUMSTICK,
        "Drumstick",
        &[("Health bonus", "+20")],
        "Increases health.",
        "images/items/icon_drumstick.gif",
        "images/items/detail_drumstick.jpg",
    ));
    manager.register_item_type(item(
        BW_ITEM_TYPE_SPIDER_LEG,
        "Spider leg",
        &[("Durability", "100")],
        "A leg from a giant spider.",
        "images/items/icon_spiderleg.gif",
        "images/items/detail_spiderleg.jpg",
    ));
    manager.register_item_type(item(
        BW_ITEM_TYPE_BINOCULARS,
        "Binoculars",
        &[],
        "Allows you to see into the distance.",
        "images/items/icon_binoculars1.gif",
        "images/items/detail_binoculars1.jpg",
    ));
    manager.register_item_type(item(
        BW_ITEM_TYPE_SWORD,
        "Sword",
        &[
            ("Accuracy", "80%"),
            ("Speed", "3s"),
            ("Power", "40"),
            ("Type", "melee"),
        ],
        "An elegant weapon for a civilised age. Perfect for dueling mercenaries.",
        "images/items/icon_sword2.gif",
        "images/items/detail_sword2.jpg",
    ));
    manager.register_item_type(item(
        BW_ITEM_TYPE_SWORD_2,
        "Bastard Sword",
        &[
            ("Accuracy", "75%"),
            ("Speed", "3.5s"),
            ("Power", "60"),
            ("Type", "melee"),
        ],
        "A (slightly larger) elegant weapon for a civilised age. Perfect for dueling mercenaries.",
        "images/items/icon_sword3.gif",
        "images/items/detail_sword3.jpg",
    ));
    manager.register_item_type(item(
        BW_ITEM_TYPE_GOBLET,
        "Goblet",
        &[("Health bonus", "+20")],
        "Healing water.",
        "images/items/icon_goblet2.gif",
        "images/items/detail_goblet2.jpg",
    ));
    manager
}

/// PHP `is_numeric`: an optionally signed integer / decimal /
/// exponential literal; leading whitespace is allowed, trailing is not,
/// and `inf` / `NaN` are not numeric.
fn php_is_numeric(raw: &str) -> bool {
    let trimmed = raw.trim_start();
    trimmed.chars().any(|c| c.is_ascii_digit())
        && matches!(trimmed.parse::<f64>(), Ok(value) if value.is_finite())
}

/// `ViewItem.php:20-29`: the `item` request parameter under the
/// reference's own validation - an empty, non-numeric or negative id is
/// the [`INVALID_ITEM_ID`] sentinel.
pub fn parse_item_id(raw: Option<&str>) -> i64 {
    let Some(raw) = raw else {
        return INVALID_ITEM_ID;
    };
    if raw.is_empty() || !php_is_numeric(raw) {
        return INVALID_ITEM_ID;
    }
    // The numeric check above guarantees the parse; `(int)` truncates
    // toward zero.
    let value = raw.trim_start().parse::<f64>().expect("numeric string");
    let truncated = value.trunc() as i64;
    if truncated < 0 {
        INVALID_ITEM_ID
    } else {
        truncated
    }
}

/// What one `ViewItem.php` request produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewItemOutcome {
    /// The -1 sentinel: the body renders [`INVALID_ITEM_MESSAGE`].
    Invalid,
    /// The id is not in the catalog: the page redirects
    /// (`ViewItem.php:41`, kept verbatim - without the `.php` suffix).
    Redirect(String),
    /// A known item: the title becomes the item name
    /// (`ViewItem.php:46`).
    Item {
        title: String,
        img_href: String,
        description: String,
    },
}

/// `ViewItemPage::initialise` (`ViewItem.php:18-51`). The reference
/// looks the id up in `$GLOBALS['BW_ITEM_TYPES']`, which nothing in the
/// tree ever defines - the page could only ever redirect. The port
/// checks the evident intent, the same [`ItemTypeManager`] catalog, with
/// the thumbnail taken from the catalog entry's icon.
pub fn view_item(
    manager: &ItemTypeManager,
    item: Option<&str>,
    referrer: Option<&str>,
) -> ViewItemOutcome {
    let item_id = parse_item_id(item);
    if item_id == INVALID_ITEM_ID {
        return ViewItemOutcome::Invalid;
    }
    let Ok(type_id) = u32::try_from(item_id) else {
        return ViewItemOutcome::Redirect(format!(
            "ViewItem?referrer={}",
            referrer.unwrap_or_default()
        ));
    };
    let Some(item_type) = manager.get(type_id) else {
        return ViewItemOutcome::Redirect(format!(
            "ViewItem?referrer={}",
            referrer.unwrap_or_default()
        ));
    };
    ViewItemOutcome::Item {
        title: item_type.name.clone(),
        img_href: item_type.icon_img_url.clone(),
        description: item_type.description.clone(),
    }
}

/// `ItemViewer::acquireInfo` (`ItemViewer.php:27-57`): the viewer's
/// resolved view of one catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemViewerInfo {
    pub item_type_name: String,
    /// The detail image (`ItemViewer.php:35`).
    pub image_url: String,
    pub description: String,
    /// The stat rows in the catalog's order.
    pub statistics: Vec<(String, String)>,
}

/// `ItemViewer::acquireInfo` (`ItemViewer.php:27-57`): resolve the
/// viewer's view of a catalog entry.
pub fn acquire_info(item_type: &ItemType) -> ItemViewerInfo {
    ItemViewerInfo {
        item_type_name: item_type.name.clone(),
        image_url: item_type.detail_img_url.clone(),
        description: item_type.description.clone(),
        statistics: item_type.stats.clone(),
    }
}

impl ItemViewerInfo {
    /// `ItemViewer::output` (`ItemViewer.php:62-73`): with a known
    /// device resolution the detail image routes through the resizer at
    /// 75% of the width.
    pub fn output_image_url(&self, display_width: Option<f32>) -> String {
        match display_width {
            Some(width) => format!(
                "Image.php?path={}&max_width={}",
                self.image_url,
                0.75 * width
            ),
            None => self.image_url.clone(),
        }
    }
}

/// PHP's implicit numeric-string array-key coercion for `$_REQUEST`
/// serials (`Inventory.php:122`, `:151`).
pub(crate) fn coerce_serial(raw: &str) -> Option<u64> {
    raw.parse::<u64>().ok()
}

/// The `Inventory.php` render data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryRender {
    /// The `goldRow` div (`Inventory.php:161-166`).
    pub gold_line: String,
    /// One row per unlocked item, in reply order - locked rows
    /// (`lockHandle != -1`) are skipped (`Inventory.php:177-181`).
    pub rows: Vec<InventoryRow>,
    /// The serial-keyed item map (`Inventory.php:59`), which the
    /// auction action arm reads
    /// (`Inventory.php:122` - see [`crate::web_auctions`]).
    pub items: BTreeMap<u64, InventoryItem>,
    /// The optional `view_item` viewer (`Inventory.php:147-153`).
    pub viewer: Option<ItemViewerInfo>,
}

impl InventoryRender {
    /// `Inventory.php:275-278`: `rowNum == 0` renders the
    /// `Inventory is empty.` notice.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// One unlocked inventory row (`Inventory.php:182-204`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryRow {
    pub serial: u64,
    pub item_type: u32,
    /// The catalog name, `None` for an id outside the catalog.
    pub name: Option<String>,
    pub icon_img_url: Option<String>,
}

/// `InventoryPage` through the first render (`Inventory.php:29-60`,
/// `:139-211`, `:275-278`) - the query and display half; the auction
/// action is BW-14-03-04. A failing query disconnects (`Err` carries the
/// redirect).
pub fn inventory_page(
    session: &mut WebSession,
    entity: &dyn WebEntityQueries,
    view_item: Option<&str>,
    manager: &ItemTypeManager,
) -> Result<InventoryRender, String> {
    // Inventory.php:39-47: a failing query disconnects.
    let reply = match entity.web_get_gold_and_inventory() {
        Ok(reply) => reply,
        Err(err) => {
            add_error_msg(session, err.to_string());
            return Err(DISCONNECT_PAGE_URL.to_string());
        }
    };

    // Inventory.php:51-60: the reply flattens into an ordered serial
    // list plus a serial-keyed map.
    let mut items_order = Vec::new();
    let mut items: BTreeMap<u64, InventoryItem> = BTreeMap::new();
    for item in &reply.inventory_items {
        items_order.push(item.serial);
        items.insert(item.serial, item.clone());
    }

    // Inventory.php:175-211: locked rows are skipped; the rest resolve
    // through the catalog.
    let rows = items_order
        .iter()
        .filter_map(|serial| {
            let item = &items[serial];
            if item.lock_handle != -1 {
                return None;
            }
            let info = manager.get(item.item_type);
            Some(InventoryRow {
                serial: *serial,
                item_type: item.item_type,
                name: info.map(|info| info.name.clone()),
                icon_img_url: info.map(|info| info.icon_img_url.clone()),
            })
        })
        .collect();

    // Inventory.php:147-153 + `ItemViewer::acquireInfo`: an unknown
    // serial or a serial whose item type is outside the catalog leaves
    // the viewer unset.
    let viewer = view_item
        .and_then(coerce_serial)
        .and_then(|serial| items.get(&serial))
        .and_then(|item| manager.get(item.item_type))
        .map(acquire_info);

    Ok(InventoryRender {
        gold_line: format!("Gold: {}", reply.gold_pieces),
        rows,
        items,
        viewer,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::web_session::SessionToken;

    /// An entity stub: canned replies for every query, counting the
    /// mutating calls.
    struct StubQueries {
        characters: Result<Vec<CharacterSummary>, String>,
        player: Result<PlayerInfo, String>,
        inventory: Result<GoldAndInventory, String>,
        chosen: Result<(String, u32), String>,
        created_reply: Result<(), String>,
        created: RefCell<Vec<String>>,
        choose_calls: RefCell<usize>,
    }

    impl StubQueries {
        fn account(characters: Vec<CharacterSummary>) -> Self {
            Self {
                characters: Ok(characters),
                player: Err("wrong entity".to_string()),
                inventory: Err("wrong entity".to_string()),
                chosen: Err("not chosen".to_string()),
                created_reply: Ok(()),
                created: RefCell::new(Vec::new()),
                choose_calls: RefCell::new(0),
            }
        }

        fn avatar(player: PlayerInfo) -> Self {
            Self {
                characters: Err("wrong entity".to_string()),
                player: Ok(player),
                inventory: Err("wrong entity".to_string()),
                chosen: Err("not chosen".to_string()),
                created_reply: Ok(()),
                created: RefCell::new(Vec::new()),
                choose_calls: RefCell::new(0),
            }
        }

        fn take_created(&self) -> Vec<String> {
            self.created.borrow().clone()
        }
    }

    impl WebEntityQueries for StubQueries {
        fn web_create_character(&mut self, name: &str) -> Result<(), WebError> {
            self.created.borrow_mut().push(name.to_string());
            self.created_reply.clone().map_err(WebError)
        }

        fn web_choose_character(&mut self, _name: &str) -> Result<(String, u32), WebError> {
            *self.choose_calls.borrow_mut() += 1;
            self.chosen.clone().map_err(WebError)
        }

        fn web_get_character_list(&self) -> Result<Vec<CharacterSummary>, WebError> {
            self.characters.clone().map_err(WebError)
        }

        fn web_get_player_info(&self) -> Result<PlayerInfo, WebError> {
            self.player.clone().map_err(WebError)
        }

        fn web_get_gold_and_inventory(&self) -> Result<GoldAndInventory, WebError> {
            self.inventory.clone().map_err(WebError)
        }

        fn web_create_auction(&mut self, _request: AuctionSubmission) -> Result<u64, WebError> {
            Ok(0)
        }

        fn web_bid_on_auction(&mut self, _request: BidSubmission) -> Result<String, WebError> {
            Ok(String::new())
        }
    }

    /// A session authenticated as an `Account`.
    fn account_session() -> WebSession {
        let mut session = WebSession::new();
        session.set_session_token(SessionToken {
            ip_address: "127.0.0.1".to_string(),
            last_activity: 0,
            username: "player".to_string(),
            entity_type: Some("Account".to_string()),
            entity_path: Some("entities_by_id/Account/1".to_string()),
        });
        session
    }

    fn take_errors(session: &mut WebSession) -> Vec<String> {
        session.take_flash(FLASH_KEY_ERR_MSGS).unwrap_or_default()
    }

    fn take_status(session: &mut WebSession) -> Vec<String> {
        session
            .take_flash(FLASH_KEY_STATUS_MSGS)
            .unwrap_or_default()
    }

    fn summary(name: &str, char_class: &str) -> CharacterSummary {
        CharacterSummary {
            name: name.to_string(),
            char_class: char_class.to_string(),
        }
    }

    #[test]
    fn char_class_names_mirror_the_constants() {
        assert_eq!(char_class_name("ranger"), Some("Ranger"));
        assert_eq!(char_class_name("female_warrior"), Some("Female Warrior"));
        assert_eq!(char_class_name("ped_male"), Some("Civilian"));
        assert_eq!(char_class_name("barbarian"), Some("Barbarian"));
        assert_eq!(char_class_name("orc"), None);
    }

    #[test]
    fn the_default_catalog_carries_the_eight_sample_items() {
        let manager = default_item_type_manager();
        for id in [
            BW_ITEM_TYPE_STAFF,
            BW_ITEM_TYPE_STAFF_2,
            BW_ITEM_TYPE_DRUMSTICK,
            BW_ITEM_TYPE_SPIDER_LEG,
            BW_ITEM_TYPE_BINOCULARS,
            BW_ITEM_TYPE_SWORD,
            BW_ITEM_TYPE_SWORD_2,
            BW_ITEM_TYPE_GOBLET,
        ] {
            assert!(manager.render_item_type(id), "id {id} registered");
        }
        assert!(!manager.render_item_type(8));

        let sword = manager.get(BW_ITEM_TYPE_SWORD).unwrap();
        assert_eq!(sword.name, "Sword");
        assert_eq!(
            sword.stats,
            vec![
                ("Accuracy".to_string(), "80%".to_string()),
                ("Speed".to_string(), "3s".to_string()),
                ("Power".to_string(), "40".to_string()),
                ("Type".to_string(), "melee".to_string()),
            ]
        );
        assert_eq!(
            sword.description,
            "An elegant weapon for a civilised age. Perfect for dueling mercenaries."
        );
        assert_eq!(sword.icon_img_url, "images/items/icon_sword2.gif");
        assert_eq!(sword.detail_img_url, "images/items/detail_sword2.jpg");

        // The sample data verbatim: the binoculars have no stats and the
        // goblet heals.
        assert!(
            manager
                .get(BW_ITEM_TYPE_BINOCULARS)
                .unwrap()
                .stats
                .is_empty()
        );
        assert_eq!(manager.get(BW_ITEM_TYPE_GOBLET).unwrap().stats[0].1, "+20");
        assert_eq!(
            manager.get(BW_ITEM_TYPE_STAFF_2).unwrap().name,
            "Staff of Lightning"
        );
    }

    #[test]
    fn a_fresh_manager_has_no_items() {
        let mut manager = ItemTypeManager::new();
        assert!(!manager.render_item_type(BW_ITEM_TYPE_SWORD));
        assert!(manager.get(BW_ITEM_TYPE_SWORD).is_none());
        manager.register_item_type(ItemType {
            id: BW_ITEM_TYPE_SWORD,
            name: "Sword".to_string(),
            stats: Vec::new(),
            description: String::new(),
            icon_img_url: String::new(),
            detail_img_url: String::new(),
        });
        assert!(manager.render_item_type(BW_ITEM_TYPE_SWORD));
        // A later registration replaces an earlier one.
        manager.register_item_type(ItemType {
            id: BW_ITEM_TYPE_SWORD,
            name: "Better Sword".to_string(),
            stats: Vec::new(),
            description: String::new(),
            icon_img_url: String::new(),
            detail_img_url: String::new(),
        });
        assert_eq!(
            manager.get(BW_ITEM_TYPE_SWORD).unwrap().name,
            "Better Sword"
        );
    }

    #[test]
    fn the_character_list_resolves_names_and_classes() {
        let mut session = account_session();
        let mut account = StubQueries::account(vec![
            summary("Aldric", "barbarian"),
            summary("Syra", "female_warrior"),
        ]);
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest::default(),
        );
        assert_eq!(outcome.redirect, None);
        assert_eq!(outcome.character_list.len(), 2);
        assert_eq!(outcome.character_list[0].name, "Aldric");
        assert_eq!(
            char_class_name(&outcome.character_list[0].char_class),
            Some("Barbarian")
        );
        assert_eq!(
            char_class_name(&outcome.character_list[1].char_class),
            Some("Female Warrior")
        );
    }

    #[test]
    fn an_empty_name_refuses_creation_but_still_reports_success() {
        // Characters.php:49-64: the InvalidFieldError becomes
        // "Could not create character: Character name is empty", and the
        // unconditional addStatusMsg still reports success.
        let mut session = account_session();
        let mut account = StubQueries::account(vec![summary("Aldric", "barbarian")]);
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest {
                new_character_name: Some(""),
                ..CharactersRequest::default()
            },
        );
        assert_eq!(outcome.redirect, None);
        assert_eq!(
            take_errors(&mut session),
            vec!["Could not create character: Character name is empty"]
        );
        assert_eq!(
            take_status(&mut session),
            vec!["Character creation successful"]
        );
        // The entity was never called with the empty name.
        assert!(account.take_created().is_empty());
    }

    #[test]
    fn a_failed_creation_still_reports_success() {
        // The same fall-through: an entity-side failure keeps the status
        // message.
        let mut session = account_session();
        let mut account = StubQueries::account(vec![]);
        account.created_reply = Err("duplicate name".to_string());
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest {
                new_character_name: Some("Aldric"),
                ..CharactersRequest::default()
            },
        );
        assert_eq!(outcome.redirect, None);
        assert_eq!(
            take_errors(&mut session),
            vec!["Could not create character: duplicate name"]
        );
        assert_eq!(
            take_status(&mut session),
            vec!["Character creation successful"]
        );
        assert_eq!(account.take_created(), vec!["Aldric"]);
    }

    #[test]
    fn a_successful_creation_reports_success() {
        let mut session = account_session();
        let mut account = StubQueries::account(vec![]);
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest {
                new_character_name: Some("Syra"),
                ..CharactersRequest::default()
            },
        );
        assert_eq!(take_errors(&mut session), Vec::<String>::new());
        assert_eq!(
            take_status(&mut session),
            vec!["Character creation successful"]
        );
        assert_eq!(account.take_created(), vec!["Syra"]);
        assert_eq!(outcome.redirect, None);
    }

    #[test]
    fn choosing_a_character_moves_the_session_to_the_avatar() {
        let mut session = account_session();
        let mut account = StubQueries::account(vec![summary("Aldric", "barbarian")]);
        account.chosen = Ok(("Avatar".to_string(), 7));
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest {
                character: Some("Aldric"),
                ..CharactersRequest::default()
            },
        );
        assert_eq!(take_status(&mut session), vec!["Login successful"]);
        let token = session.token().unwrap();
        assert_eq!(token.entity_type.as_deref(), Some("Avatar"));
        assert_eq!(
            token.entity_path.as_deref(),
            Some("entities_by_id/Avatar/7")
        );
        // The page redirects to the welcome page, and the list is still
        // fetched after the redirect was set (Characters.php:89-99).
        assert_eq!(
            outcome.redirect.as_deref(),
            Some(CHARACTER_WELCOME_PAGE_URL)
        );
        assert_eq!(outcome.character_list.len(), 1);
        assert_eq!(*account.choose_calls.borrow(), 1);
    }

    #[test]
    fn a_failing_choice_disconnects() {
        let mut session = account_session();
        let mut account = StubQueries::account(vec![]);
        account.chosen = Err("no such character".to_string());
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest {
                character: Some("Ghost"),
                ..CharactersRequest::default()
            },
        );
        assert_eq!(outcome.redirect.as_deref(), Some(DISCONNECT_PAGE_URL));
        assert_eq!(outcome.character_list, Vec::new());
        assert_eq!(take_errors(&mut session), vec!["no such character"]);
        assert_eq!(take_status(&mut session), Vec::<String>::new());
    }

    #[test]
    fn a_choice_on_a_session_without_a_token_still_redirects() {
        // A token-less session cannot be restamped; the page still
        // reports the login and redirects (PHP would fatal on the null
        // token here, the port is defensive).
        let mut session = WebSession::new();
        let mut account = StubQueries::account(vec![]);
        account.chosen = Ok(("Avatar".to_string(), 3));
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest {
                character: Some("Syra"),
                ..CharactersRequest::default()
            },
        );
        assert_eq!(
            outcome.redirect.as_deref(),
            Some(CHARACTER_WELCOME_PAGE_URL)
        );
        assert_eq!(take_status(&mut session), vec!["Login successful"]);
        assert!(session.token().is_none());
    }

    #[test]
    fn a_page_without_an_account_disconnects() {
        let mut session = account_session();
        let outcome = characters_page(&mut session, None, CharactersRequest::default());
        assert_eq!(outcome.redirect.as_deref(), Some(DISCONNECT_PAGE_URL));
        assert_eq!(take_errors(&mut session), vec!["Unable to access account"]);
    }

    #[test]
    fn an_avatar_is_sent_to_the_welcome_page() {
        let mut session = account_session();
        let token = session.token().unwrap().clone();
        session.set_session_token(SessionToken {
            entity_type: Some("Avatar".to_string()),
            ..token
        });
        let mut account = StubQueries::account(vec![summary("Aldric", "barbarian")]);
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest::default(),
        );
        assert_eq!(
            outcome.redirect.as_deref(),
            Some(CHARACTER_WELCOME_PAGE_URL)
        );
        // The page bailed before any query.
        assert!(outcome.character_list.is_empty());
        assert!(account.take_created().is_empty());
        assert_eq!(*account.choose_calls.borrow(), 0);
    }

    #[test]
    fn logout_returns_to_the_login_page() {
        let mut session = account_session();
        let outcome = characters_page(
            &mut session,
            None,
            CharactersRequest {
                logout: Some("1"),
                ..CharactersRequest::default()
            },
        );
        // The logout check runs before the account check.
        assert_eq!(outcome.redirect.as_deref(), Some("Login.php?logout=1"));
        assert_eq!(take_errors(&mut session), Vec::<String>::new());
    }

    #[test]
    fn a_failing_character_list_disconnects() {
        let mut session = account_session();
        let mut account = StubQueries::account(vec![]);
        account.characters = Err("db unreachable".to_string());
        let outcome = characters_page(
            &mut session,
            Some(&mut account),
            CharactersRequest::default(),
        );
        assert_eq!(outcome.redirect.as_deref(), Some(DISCONNECT_PAGE_URL));
        assert_eq!(outcome.character_list, Vec::new());
        assert_eq!(take_errors(&mut session), vec!["db unreachable"]);
    }

    fn player(online: bool) -> PlayerInfo {
        // (roll, pitch, yaw) at the reference's own imprecise literals:
        // π, π/6 and π/2 to four decimals.
        #[allow(clippy::approx_constant)]
        PlayerInfo {
            char_class: "ranger".to_string(),
            database_id: 42,
            player_name: "Syra".to_string(),
            health: 50,
            max_health: 100,
            frags: 12,
            position: [1.0, 2.25, 3.5],
            direction: [3.1415, 0.5236, 1.5708],
            online,
        }
    }

    #[test]
    fn the_character_page_reports_statistics_in_reference_order() {
        let mut session = account_session();
        let avatar = StubQueries::avatar(player(true));
        let details = character_page(&mut session, &avatar).unwrap();
        assert_eq!(details.title, "Syra");
        let statistics = details.statistics;
        assert_eq!(statistics.len(), 10);
        assert_eq!(statistics[0], ("Class", "Ranger".to_string()));
        assert_eq!(statistics[1], ("Strength", "23".to_string()));
        assert_eq!(statistics[2], ("Dexterity", "15".to_string()));
        assert_eq!(statistics[3], ("Defense", "21".to_string()));
        assert_eq!(statistics[4], ("Intelligence", "18".to_string()));
        assert_eq!(statistics[5], ("Health", "50\u{a0}/\u{a0}100".to_string()));
        assert_eq!(statistics[6], ("Frags", "12".to_string()));
        assert_eq!(statistics[7], ("Status", "online".to_string()));
        // Position prints as a %.01f triple.
        assert_eq!(statistics[8], ("Position", "(1.0, 2.2, 3.5)".to_string()));
        // Direction arrives (roll, pitch, yaw), prints yaw first, in
        // degrees: 1.5708 rad ≈ 90.0°, 0.5236 ≈ 30.0°, 3.1415 ≈ 180.0°.
        assert_eq!(
            statistics[9],
            (
                "Direction",
                "(yaw=90.0, pitch=30.0, roll=180.0) [deg]".to_string()
            )
        );
        assert_eq!(take_errors(&mut session), Vec::<String>::new());
    }

    #[test]
    fn an_offline_character_omits_position_and_direction() {
        let mut session = account_session();
        let avatar = StubQueries::avatar(player(false));
        let details = character_page(&mut session, &avatar).unwrap();
        assert_eq!(details.statistics.len(), 8);
        assert_eq!(details.statistics[7], ("Status", "offline".to_string()));
    }

    #[test]
    fn a_failing_player_info_disconnects() {
        let mut session = account_session();
        let mut avatar = StubQueries::avatar(player(true));
        avatar.player = Err("entity gone".to_string());
        let redirect = character_page(&mut session, &avatar).unwrap_err();
        assert_eq!(redirect, DISCONNECT_PAGE_URL);
        assert_eq!(take_errors(&mut session), vec!["entity gone"]);
    }

    #[test]
    fn item_ids_follow_php_numeric_coercion() {
        assert_eq!(parse_item_id(None), INVALID_ITEM_ID);
        assert_eq!(parse_item_id(Some("")), INVALID_ITEM_ID);
        assert_eq!(parse_item_id(Some("abc")), INVALID_ITEM_ID);
        assert_eq!(parse_item_id(Some("-3")), INVALID_ITEM_ID);
        assert_eq!(parse_item_id(Some("inf")), INVALID_ITEM_ID);
        assert_eq!(parse_item_id(Some("NaN")), INVALID_ITEM_ID);
        // Trailing whitespace is not numeric; leading is.
        assert_eq!(parse_item_id(Some("6 ")), INVALID_ITEM_ID);
        assert_eq!(parse_item_id(Some(" 6")), 6);
        // `(int)` truncates the fractional part.
        assert_eq!(parse_item_id(Some("5.9")), 5);
        assert_eq!(parse_item_id(Some("7")), 7);
    }

    #[test]
    fn view_item_shows_a_known_item() {
        let manager = default_item_type_manager();
        let outcome = view_item(&manager, Some("7"), Some("Inventory.php"));
        assert_eq!(
            outcome,
            ViewItemOutcome::Item {
                title: "Sword".to_string(),
                img_href: "images/items/icon_sword2.gif".to_string(),
                description:
                    "An elegant weapon for a civilised age. Perfect for dueling mercenaries."
                        .to_string(),
            }
        );
    }

    #[test]
    fn view_item_redirects_an_unknown_item() {
        let manager = default_item_type_manager();
        assert_eq!(
            view_item(&manager, Some("99"), Some("Inventory.php")),
            ViewItemOutcome::Redirect("ViewItem?referrer=Inventory.php".to_string())
        );
        // No referrer at all concatenates the empty string.
        assert_eq!(
            view_item(&manager, Some("99"), None),
            ViewItemOutcome::Redirect("ViewItem?referrer=".to_string())
        );
        // An id beyond the key type cannot be in the catalog either.
        assert_eq!(
            view_item(&manager, Some("4294967296"), None),
            ViewItemOutcome::Redirect("ViewItem?referrer=".to_string())
        );
    }

    #[test]
    fn view_item_reports_an_invalid_choice() {
        let manager = default_item_type_manager();
        assert_eq!(
            view_item(&manager, Some("not-an-id"), Some("Inventory.php")),
            ViewItemOutcome::Invalid
        );
    }

    #[test]
    fn the_viewer_resolves_a_catalog_entry_and_resizes_for_the_device() {
        let manager = default_item_type_manager();
        let info = acquire_info(manager.get(BW_ITEM_TYPE_DRUMSTICK).unwrap());
        assert_eq!(info.item_type_name, "Drumstick");
        assert_eq!(info.image_url, "images/items/detail_drumstick.jpg");
        assert_eq!(info.description, "Increases health.");
        assert_eq!(
            info.statistics,
            vec![("Health bonus".to_string(), "+20".to_string())]
        );
        // Without a known resolution the raw detail image is used.
        assert_eq!(
            info.output_image_url(None),
            "images/items/detail_drumstick.jpg"
        );
        // With one, the image routes through the resizer at 75% width.
        assert_eq!(
            info.output_image_url(Some(800.0)),
            "Image.php?path=images/items/detail_drumstick.jpg&max_width=600"
        );
        assert_eq!(
            info.output_image_url(Some(333.5)),
            "Image.php?path=images/items/detail_drumstick.jpg&max_width=250.125"
        );
    }

    fn inventory_reply(items: Vec<InventoryItem>) -> GoldAndInventory {
        GoldAndInventory {
            gold_pieces: 250,
            inventory_items: items,
            locked_items: Vec::new(),
        }
    }

    #[test]
    fn the_inventory_lists_gold_and_unlocked_items() {
        let mut session = account_session();
        let mut avatar = StubQueries::avatar(player(true));
        avatar.inventory = Ok(inventory_reply(vec![
            InventoryItem {
                serial: 1,
                item_type: BW_ITEM_TYPE_SWORD,
                lock_handle: -1,
            },
            InventoryItem {
                serial: 2,
                item_type: BW_ITEM_TYPE_DRUMSTICK,
                lock_handle: -1,
            },
            // A locked row is hidden from the list.
            InventoryItem {
                serial: 3,
                item_type: BW_ITEM_TYPE_GOBLET,
                lock_handle: 42,
            },
        ]));
        let render =
            inventory_page(&mut session, &avatar, None, &default_item_type_manager()).unwrap();
        assert_eq!(render.gold_line, "Gold: 250");
        assert!(!render.is_empty());
        assert_eq!(render.rows.len(), 2);
        assert_eq!(render.rows[0].serial, 1);
        assert_eq!(render.rows[0].name.as_deref(), Some("Sword"));
        assert_eq!(
            render.rows[0].icon_img_url.as_deref(),
            Some("images/items/icon_sword2.gif")
        );
        assert_eq!(render.rows[1].serial, 2);
        assert_eq!(render.rows[1].name.as_deref(), Some("Drumstick"));
        // The serials stay in reply order, locks skipped in place.
        assert_eq!(
            render.rows.iter().map(|row| row.serial).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(take_errors(&mut session), Vec::<String>::new());
    }

    #[test]
    fn an_all_locked_inventory_reports_itself_empty() {
        let mut session = account_session();
        let mut avatar = StubQueries::avatar(player(true));
        avatar.inventory = Ok(inventory_reply(vec![InventoryItem {
            serial: 9,
            item_type: BW_ITEM_TYPE_GOBLET,
            lock_handle: 7,
        }]));
        let render =
            inventory_page(&mut session, &avatar, None, &default_item_type_manager()).unwrap();
        assert!(render.is_empty());
        assert_eq!(render.rows, Vec::new());
        assert_eq!(render.gold_line, "Gold: 250");
    }

    #[test]
    fn a_failing_inventory_disconnects() {
        let mut session = account_session();
        let mut avatar = StubQueries::avatar(player(true));
        avatar.inventory = Err("db unreachable".to_string());
        let redirect =
            inventory_page(&mut session, &avatar, None, &default_item_type_manager()).unwrap_err();
        assert_eq!(redirect, DISCONNECT_PAGE_URL);
        assert_eq!(take_errors(&mut session), vec!["db unreachable"]);
    }

    #[test]
    fn the_inventory_viewer_resolves_the_viewed_item() {
        let mut session = account_session();
        let mut avatar = StubQueries::avatar(player(true));
        avatar.inventory = Ok(inventory_reply(vec![
            InventoryItem {
                serial: 2,
                item_type: BW_ITEM_TYPE_DRUMSTICK,
                lock_handle: -1,
            },
            // A serial whose item type is outside the catalog cannot be
            // viewed.
            InventoryItem {
                serial: 3,
                item_type: 99,
                lock_handle: -1,
            },
        ]));
        // The `view_item` request key is a numeric-string serial.
        let render = inventory_page(
            &mut session,
            &avatar,
            Some("2"),
            &default_item_type_manager(),
        )
        .unwrap();
        let viewer = render.viewer.unwrap();
        assert_eq!(viewer.item_type_name, "Drumstick");
        assert_eq!(viewer.image_url, "images/items/detail_drumstick.jpg");
        assert_eq!(viewer.description, "Increases health.");

        // An unknown serial, a non-numeric serial and an uncatalogued
        // item type all leave the viewer unset.
        for raw in [Some("99"), Some("abc"), None] {
            let render =
                inventory_page(&mut session, &avatar, raw, &default_item_type_manager()).unwrap();
            assert_eq!(render.viewer, None, "serial {raw:?}");
        }
        let render = inventory_page(
            &mut session,
            &avatar,
            Some("3"),
            &default_item_type_manager(),
        )
        .unwrap();
        assert_eq!(render.viewer, None);
    }
}
