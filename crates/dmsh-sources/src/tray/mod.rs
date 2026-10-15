//! System tray: a StatusNotifierItem host. If another program runs the
//! `org.kde.StatusNotifierWatcher` (e.g. a Quickshell bar), dmsh registers with it as a
//! host; otherwise dmsh runs the watcher itself. Item menus come over
//! `com.canonical.dbusmenu` and are shown inside the right island.

mod theme;

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use zbus::{
    Connection,
    fdo::DBusProxy,
    message::Header,
    object_server::SignalEmitter,
    proxy::CacheProperties,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use crate::{Emitter, Event};
use theme::IconFinder;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";

/// A tray icon as the shell needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrayItem {
    /// The item's bus address (`name/path`); stable while the app runs.
    pub id: String,
    pub title: String,
    pub icon: Option<TrayIcon>,
    pub needs_attention: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrayIcon {
    /// Premultiplied RGBA, with a hash of the pixels for cache keys.
    Pixels {
        width: u32,
        height: u32,
        rgba: Arc<Vec<u8>>,
        hash: u64,
    },
    File(PathBuf),
}

/// An item's context menu (a tree).
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub item: String,
    pub title: String,
    pub entries: Vec<MenuEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    /// `Some(checked)` for checkmark and radio items.
    pub toggle: Option<bool>,
    pub children: Vec<MenuEntry>,
}

/// What the shell asks of the tray.
#[derive(Debug)]
pub enum Command {
    Activate { item: String },
    SecondaryActivate { item: String },
    OpenMenu { item: String },
    MenuClick { item: String, entry: i32 },
}

#[zbus::proxy(
    interface = "org.kde.StatusNotifierWatcher",
    default_service = "org.kde.StatusNotifierWatcher",
    default_path = "/StatusNotifierWatcher"
)]
trait Watcher {
    fn register_status_notifier_host(&self, service: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> zbus::Result<Vec<String>>;
    #[zbus(signal)]
    fn status_notifier_item_registered(&self, service: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn status_notifier_item_unregistered(&self, service: String) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.kde.StatusNotifierItem")]
trait Item {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn title(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_theme_path(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_pixmap(&self) -> zbus::Result<Vec<(i32, i32, Vec<u8>)>>;
    #[zbus(property)]
    fn attention_icon_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> zbus::Result<Vec<(i32, i32, Vec<u8>)>>;
    #[zbus(property)]
    fn item_is_menu(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn menu(&self) -> zbus::Result<OwnedObjectPath>;
    fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;
    fn secondary_activate(&self, x: i32, y: i32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_icon(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_attention_icon(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_title(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_status(&self, status: String) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "com.canonical.dbusmenu")]
trait DbusMenu {
    #[allow(clippy::type_complexity)]
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>))>;
    fn event(&self, id: i32, event_id: &str, data: &Value<'_>, timestamp: u32) -> zbus::Result<()>;
    fn about_to_show(&self, id: i32) -> zbus::Result<bool>;
}

/// Everything known about one item.
#[derive(Clone, Debug)]
struct Info {
    item: TrayItem,
    visible: bool,
    item_is_menu: bool,
    menu: Option<String>,
}

enum Change {
    Added(String),
    Removed(String),
    Updated(String, Box<Info>),
}

pub(crate) async fn run(
    emitter: Emitter,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<()> {
    let conn = Connection::session().await?;
    let icons = Arc::new(Mutex::new(IconFinder::new()));
    let (tx, mut changes) = mpsc::unbounded_channel::<Change>();
    connect_watcher(&conn, tx.clone()).await?;
    let dbus = DBusProxy::new(&conn).await?;
    let mut watcher_owner = dbus
        .receive_name_owner_changed_with_args(&[(0, WATCHER)])
        .await?;
    let mut owners = dbus.receive_name_owner_changed().await?;

    let mut followers: HashMap<String, AbortHandle> = HashMap::new();
    let mut infos: BTreeMap<String, Info> = BTreeMap::new();
    // Registered address → normalized id (unique bus name + path), so an app that
    // registers under both its well-known and unique name appears once.
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut shown: Vec<TrayItem> = Vec::new();
    loop {
        tokio::select! {
            Some(change) = changes.recv() => match change {
                Change::Added(registered) => {
                    let id = normalize(&dbus, &registered).await;
                    aliases.insert(registered, id.clone());
                    if let std::collections::hash_map::Entry::Vacant(slot) = followers.entry(id) {
                        let id = slot.key().clone();
                        slot.insert(follow(conn.clone(), id, icons.clone(), tx.clone()));
                    }
                }
                Change::Removed(registered) => {
                    let id = aliases.remove(&registered).unwrap_or(registered);
                    aliases.retain(|_, v| *v != id);
                    if let Some(handle) = followers.remove(&id) {
                        handle.abort();
                    }
                    infos.remove(&id);
                }
                Change::Updated(id, info) => {
                    if followers.contains_key(&id) {
                        infos.insert(id, *info);
                    }
                }
            },
            // The watcher we registered with went away (e.g. its bar exited): become the
            // watcher ourselves; apps re-register with it.
            Some(signal) = watcher_owner.next() => {
                let gone = signal.args().is_ok_and(|a| a.new_owner().is_none());
                if gone {
                    // Known items stay: many apps (Qt) never re-register with a new
                    // watcher. They're dropped when their app leaves the bus.
                    log::info!("tray: the StatusNotifierWatcher left; taking over");
                    if let Err(e) = connect_watcher(&conn, tx.clone()).await {
                        log::warn!("tray: taking over the watcher: {e:#}");
                    }
                }
                continue;
            }
            // An app left the bus: drop its items even if no watcher says so.
            Some(signal) = owners.next() => {
                let Ok(args) = signal.args() else { continue };
                if args.new_owner().is_some() || !args.name().starts_with(':') {
                    continue;
                }
                let gone = args.name().to_string();
                let ids: Vec<String> = followers
                    .keys()
                    .filter(|id| address(id).0 == gone)
                    .cloned()
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                for id in ids {
                    if let Some(handle) = followers.remove(&id) {
                        handle.abort();
                    }
                    infos.remove(&id);
                    aliases.retain(|_, v| *v != id);
                }
            }
            Some(command) = commands.recv() => {
                if let Err(e) = handle(&conn, &infos, command, &emitter).await {
                    log::warn!("tray: {e:#}");
                }
                continue;
            }
            else => break,
        }
        let items: Vec<TrayItem> = infos
            .values()
            .filter(|i| i.visible)
            .map(|i| i.item.clone())
            .collect();
        if items != shown {
            emitter.emit(Event::Tray(items.clone()));
            shown = items;
        }
    }
    anyhow::bail!("session bus closed")
}

/// Registers as a host with the running watcher, starting our own if there is none.
async fn connect_watcher(conn: &Connection, tx: mpsc::UnboundedSender<Change>) -> Result<()> {
    let dbus = DBusProxy::new(conn).await?;
    if !dbus.name_has_owner(WATCHER.try_into()?).await? {
        let watcher = OwnWatcher {
            items: Arc::new(Mutex::new(Vec::new())),
        };
        let items = watcher.items.clone();
        conn.object_server().at(WATCHER_PATH, watcher).await?;
        conn.request_name(WATCHER).await?;
        log::info!("tray: running the StatusNotifierWatcher");
        tokio::spawn(prune_vanished(conn.clone(), items));
    }

    let host = format!("org.kde.StatusNotifierHost-{}-dmsh", std::process::id());
    // The host name serves no objects, but starting the object server first avoids a
    // zbus warning about calls arriving before it exists.
    let _ = conn.object_server();
    conn.request_name(host.as_str()).await?;
    let watcher = WatcherProxy::new(conn).await?;
    let mut registered = watcher.receive_status_notifier_item_registered().await?;
    let mut unregistered = watcher.receive_status_notifier_item_unregistered().await?;
    watcher.register_status_notifier_host(&host).await?;
    for item in watcher.registered_status_notifier_items().await? {
        let _ = tx.send(Change::Added(item));
    }
    tokio::spawn(async move {
        loop {
            let change = tokio::select! {
                Some(s) = registered.next() => s.args().map(|a| Change::Added(a.service)),
                Some(s) = unregistered.next() => s.args().map(|a| Change::Removed(a.service)),
                else => return,
            };
            if let Ok(change) = change
                && tx.send(change).is_err()
            {
                return;
            }
        }
    });
    Ok(())
}

/// Rewrites an item address to use the owner's unique bus name.
async fn normalize(dbus: &DBusProxy<'_>, registered: &str) -> String {
    let (name, path) = address(registered);
    if name.starts_with(':') {
        return format!("{name}{path}");
    }
    let owner = match zbus::names::BusName::try_from(name) {
        Ok(bus_name) => dbus.get_name_owner(bus_name).await.ok(),
        Err(_) => None,
    };
    match owner {
        Some(unique) => format!("{unique}{path}"),
        None => format!("{name}{path}"),
    }
}

/// Splits an item address into bus name and object path.
fn address(id: &str) -> (&str, &str) {
    match id.find('/') {
        Some(i) => (&id[..i], &id[i..]),
        None => (id, DEFAULT_ITEM_PATH),
    }
}

async fn item_proxy<'a>(conn: &Connection, id: &'a str) -> zbus::Result<ItemProxy<'a>> {
    let (name, path) = address(id);
    ItemProxy::builder(conn)
        .destination(name)?
        .path(path)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

/// Reads an item and re-reads it whenever it signals a change, until aborted.
fn follow(
    conn: Connection,
    id: String,
    icons: Arc<Mutex<IconFinder>>,
    tx: mpsc::UnboundedSender<Change>,
) -> AbortHandle {
    tokio::spawn(async move {
        let result: zbus::Result<()> = async {
            let proxy = item_proxy(&conn, &id).await?;
            let mut icon = proxy.receive_new_icon().await?;
            let mut attention = proxy.receive_new_attention_icon().await?;
            let mut title = proxy.receive_new_title().await?;
            let mut status = proxy.receive_new_status().await?;
            loop {
                let info = read(&proxy, &id, &icons).await;
                if tx
                    .send(Change::Updated(id.clone(), Box::new(info)))
                    .is_err()
                {
                    return Ok(());
                }
                tokio::select! {
                    Some(_) = icon.next() => {}
                    Some(_) = attention.next() => {}
                    Some(_) = title.next() => {}
                    Some(_) = status.next() => {}
                    else => return Ok(()),
                }
                // Animated icons and bursts of signals: read once they settle.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        .await;
        if let Err(e) = result {
            log::debug!("tray item {id}: {e}");
        }
    })
    .abort_handle()
}

async fn read(proxy: &ItemProxy<'_>, id: &str, icons: &Mutex<IconFinder>) -> Info {
    let status = proxy.status().await.unwrap_or_default();
    let needs_attention = status == "NeedsAttention";
    let (name, pixmap) = if needs_attention {
        (
            proxy.attention_icon_name().await,
            proxy.attention_icon_pixmap().await,
        )
    } else {
        (proxy.icon_name().await, proxy.icon_pixmap().await)
    };
    let theme_path = proxy.icon_theme_path().await.ok();
    let file = name
        .ok()
        .and_then(|n| icons.lock().ok()?.find(&n, theme_path.as_deref()));
    let icon = file
        .map(TrayIcon::File)
        .or_else(|| pixmap.ok().and_then(|p| best_pixmap(&p)));
    let title = match proxy.title().await {
        Ok(t) if !t.is_empty() => t,
        _ => proxy.id().await.unwrap_or_default(),
    };
    Info {
        item: TrayItem {
            id: id.to_owned(),
            title,
            icon,
            needs_attention,
        },
        visible: status != "Passive",
        item_is_menu: proxy.item_is_menu().await.unwrap_or(false),
        menu: proxy
            .menu()
            .await
            .ok()
            .map(|p| p.to_string())
            .filter(|p| p != "/"),
    }
}

/// Picks the smallest pixmap that is at least 32 px (else the largest) and converts it
/// from ARGB32 in network byte order to premultiplied RGBA.
fn best_pixmap(pixmaps: &[(i32, i32, Vec<u8>)]) -> Option<TrayIcon> {
    let valid = pixmaps
        .iter()
        .filter(|(w, h, d)| *w > 0 && *h > 0 && d.len() == (*w * *h * 4) as usize);
    let (w, h, data) = valid
        .clone()
        .filter(|(w, _, _)| *w >= 32)
        .min_by_key(|(w, _, _)| *w)
        .or_else(|| valid.max_by_key(|(w, _, _)| *w))?;
    let mut rgba = Vec::with_capacity(data.len());
    for px in data.as_chunks::<4>().0 {
        let [a, r, g, b] = *px;
        let premul = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
        rgba.extend_from_slice(&[premul(r), premul(g), premul(b), a]);
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    Some(TrayIcon::Pixels {
        width: *w as u32,
        height: *h as u32,
        rgba: Arc::new(rgba),
        hash: hasher.finish(),
    })
}

async fn handle(
    conn: &Connection,
    infos: &BTreeMap<String, Info>,
    command: Command,
    emitter: &Emitter,
) -> Result<()> {
    match command {
        Command::Activate { item } => {
            let info = infos.get(&item).context("no such tray item")?;
            // Menu-only items, and apps that don't implement Activate, get their menu.
            let activated =
                !info.item_is_menu && item_proxy(conn, &item).await?.activate(0, 0).await.is_ok();
            if !activated {
                open_menu(conn, info, emitter).await?;
            }
        }
        Command::SecondaryActivate { item } => {
            item_proxy(conn, &item)
                .await?
                .secondary_activate(0, 0)
                .await?;
        }
        Command::OpenMenu { item } => {
            let info = infos.get(&item).context("no such tray item")?;
            open_menu(conn, info, emitter).await?;
        }
        Command::MenuClick { item, entry } => {
            let info = infos.get(&item).context("no such tray item")?;
            let menu = menu_proxy(conn, info).await?;
            menu.event(entry, "clicked", &Value::I32(0), 0).await?;
        }
    }
    Ok(())
}

async fn menu_proxy<'a>(conn: &Connection, info: &'a Info) -> Result<DbusMenuProxy<'a>> {
    let path = info.menu.as_deref().context("item has no menu")?;
    let (name, _) = address(&info.item.id);
    Ok(DbusMenuProxy::builder(conn)
        .destination(name)?
        .path(path)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?)
}

async fn open_menu(conn: &Connection, info: &Info, emitter: &Emitter) -> Result<()> {
    let menu = menu_proxy(conn, info).await?;
    // Lets apps that build menus lazily fill them in; failures are harmless.
    let _ = menu.about_to_show(0).await;
    let (_, (_, _, children)) = menu.get_layout(0, -1, &[]).await?;
    emitter.emit(Event::Menu(Some(Menu {
        item: info.item.id.clone(),
        title: info.item.title.clone(),
        entries: parse_entries(children),
    })));
    Ok(())
}

fn parse_entries(children: Vec<OwnedValue>) -> Vec<MenuEntry> {
    children.into_iter().filter_map(parse_entry).collect()
}

fn parse_entry(value: OwnedValue) -> Option<MenuEntry> {
    let (id, props, children): (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>) =
        value.try_into().ok()?;
    let string = |k: &str| {
        props
            .get(k)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
    };
    let boolean = |k: &str| {
        props
            .get(k)
            .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
    };
    if boolean("visible") == Some(false) {
        return None;
    }
    let separator = string("type").as_deref() == Some("separator");
    let toggle = string("toggle-type").filter(|t| !t.is_empty()).map(|_| {
        props
            .get("toggle-state")
            .and_then(|v| i32::try_from(v.try_clone().ok()?).ok())
            == Some(1)
    });
    Some(MenuEntry {
        id,
        label: strip_mnemonic(&string("label").unwrap_or_default()),
        enabled: boolean("enabled").unwrap_or(true),
        separator,
        toggle,
        children: parse_entries(children),
    })
}

/// `_File` → `File`, `__` → `_`.
fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Our own watcher, used when nothing else provides one.
struct OwnWatcher {
    items: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl OwnWatcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        // Items may register a bare object path; it belongs to the caller.
        let id = if service.starts_with('/') {
            let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
            format!("{sender}{service}")
        } else {
            service.to_owned()
        };
        let added = {
            let mut items = self.items.lock().expect("lock poisoned");
            let new = !items.contains(&id);
            if new {
                items.push(id.clone());
            }
            new
        };
        if added {
            let _ = Self::status_notifier_item_registered(&emitter, &id).await;
        }
    }

    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().expect("lock poisoned").clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Unregisters items whose app left the bus (our own watcher only).
async fn prune_vanished(conn: Connection, items: Arc<Mutex<Vec<String>>>) -> zbus::Result<()> {
    let dbus = DBusProxy::new(&conn).await?;
    let mut owners = dbus.receive_name_owner_changed().await?;
    let iface = conn
        .object_server()
        .interface::<_, OwnWatcher>(WATCHER_PATH)
        .await?;
    while let Some(signal) = owners.next().await {
        let Ok(args) = signal.args() else { continue };
        if args.new_owner().is_some() {
            continue;
        }
        let gone = args.name().to_string();
        let removed: Vec<String> = {
            let mut items = items.lock().expect("lock poisoned");
            let (removed, kept) = items.drain(..).partition(|i| address(i).0 == gone);
            *items = kept;
            removed
        };
        for id in removed {
            let _ =
                OwnWatcher::status_notifier_item_unregistered(iface.signal_emitter(), &id).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(
            address(":1.50/org/blueman/sni"),
            (":1.50", "/org/blueman/sni")
        );
        assert_eq!(address("org.kde.Foo"), ("org.kde.Foo", DEFAULT_ITEM_PATH));
    }

    #[test]
    fn mnemonics() {
        assert_eq!(strip_mnemonic("_Quit"), "Quit");
        assert_eq!(strip_mnemonic("Save__As"), "Save_As");
    }

    #[test]
    fn pixmap_choice_and_premultiplication() {
        let small = (16, 16, vec![0u8; 16 * 16 * 4]);
        let mut big = (48, 48, vec![0u8; 48 * 48 * 4]);
        big.2[..4].copy_from_slice(&[128, 255, 0, 0]); // half-transparent red
        let Some(TrayIcon::Pixels { width, rgba, .. }) = best_pixmap(&[small, big]) else {
            panic!("expected pixels")
        };
        assert_eq!(width, 48);
        assert_eq!(&rgba[..4], &[128, 0, 0, 128]);
        assert!(
            best_pixmap(&[(4, 4, vec![0; 3])]).is_none(),
            "bad length rejected"
        );
    }
}
