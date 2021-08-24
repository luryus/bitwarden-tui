use std::{collections::HashMap, time::Duration};

use crate::bitwarden::{
    self,
    api::{CipherItem, LoginItem},
    cipher::{Cipher, EncryptionKey, MacKey},
};
use bitwarden::api::CipherData;
use cursive::{
    theme::{BaseColor, Color},
    traits::{Nameable, Resizable},
    view::Margins,
    views::{Dialog, EditView, LayerPosition, LinearLayout, OnEventView, PaddedView, TextView},
    Cursive, View,
};
use cursive_table_view::{TableView, TableViewItem};
use itertools::Itertools;


use super::{data::UserData, item_details::item_detail_dialog, login::do_sync};

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum VaultTableColumn {
    ItemType,
    Name,
    Username,
    IsInOrganization,
}

#[derive(Clone, Debug)]
pub struct Row {
    id: String,
    name: String,
    username: String,
    url: String,
    item_type: String,
    is_in_organization: bool,
}

impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        self.id.eq(&other.id)
    }
}
impl Eq for Row {}
impl PartialOrd for Row {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(std::cmp::Ord::cmp(&self, &other))
    }
}
impl Ord for Row {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl TableViewItem<VaultTableColumn> for Row {
    fn to_column(&self, column: VaultTableColumn) -> String {
        match column {
            VaultTableColumn::ItemType => self.item_type.clone(),
            VaultTableColumn::Name => self.name.clone(),
            VaultTableColumn::Username => self.username.clone(),
            VaultTableColumn::IsInOrganization => if self.is_in_organization {
                "👥"
            } else {
                "👤"
            }
            .to_string(),
        }
    }

    fn cmp(&self, other: &Self, column: VaultTableColumn) -> std::cmp::Ordering
    where
        Self: Sized,
    {
        match column {
            VaultTableColumn::ItemType => self.item_type.cmp(&other.item_type),
            VaultTableColumn::Name => self.name.cmp(&other.name),
            VaultTableColumn::Username => self.username.cmp(&other.username),
            VaultTableColumn::IsInOrganization => {
                self.is_in_organization.cmp(&other.is_in_organization)
            }
        }
    }
}

pub fn vault_view(user_data: &mut UserData) -> impl View {
    let (enc_key, mac_key) = user_data.decrypt_keys().unwrap();
    let table = vault_table_view(user_data, &enc_key, &mac_key);

    let ll = LinearLayout::vertical()
        .child(filter_edit_view())
        .child(table)
        .weight(100)
        .child(key_hint_view());

    OnEventView::new(ll)
        .on_event('/', |siv| {
            siv.focus_name("search_edit")
                .expect("Focusing search failed");
        })
        .on_event('q', |siv| {
            let dialog = Dialog::text("Quit?")
                .dismiss_button("Cancel")
                .button("Quit", |siv| siv.quit());
            siv.add_layer(dialog);
        })
        .on_event(cursive::event::Event::CtrlChar('s'), |siv| {
            do_sync(siv);
        })
        .on_event('p', |siv| {
            copy_current_item_field(siv, Copyable::Password);
        })
        .on_event('u', |siv| {
            copy_current_item_field(siv, Copyable::Username);
        })
}

fn copy_current_item_field(siv: &mut Cursive, field: Copyable) {
    let table = siv
        .find_name::<TableView<Row, VaultTableColumn>>("vault_table")
        .unwrap();
    let row = table.borrow_item(table.item().unwrap()).unwrap();
    let ud: &mut UserData = siv.user_data().unwrap();
    let (enc_key, mac_key) = ud.decrypt_keys().unwrap();

    let vd = ud.vault_data.as_ref().unwrap();
    match (vd.get(&row.id), field) {
        (
            Some(CipherItem {
                data: CipherData::Login(LoginItem { password, .. }),
                ..
            }),
            Copyable::Password,
        ) => {
            super::clipboard::clip_exipiring_string(
                password.decrypt_to_string(&enc_key, &mac_key),
                30,
            );
            show_copy_notification(siv, "Password copied");
        }
        (
            Some(CipherItem {
                data: CipherData::Login(LoginItem { username, .. }),
                ..
            }),
            Copyable::Username,
        ) => {
            super::clipboard::clip_string(username.decrypt_to_string(&enc_key, &mac_key));
            show_copy_notification(siv, "Username copied");
        }
        _ => (),
    };
}

enum Copyable {
    Password,
    Username,
}

fn filter_edit_view() -> impl View {
    let filter_edit = EditView::new()
        .on_edit(|siv, text, _| {
            // Filter the results, update table
            if let Some(mut tv) = siv.find_name::<TableView<Row, VaultTableColumn>>("vault_table") {
                let ud: &mut UserData = siv.user_data().unwrap();

                if let Some(all_rows) = ud.vault_table_rows.as_ref() {
                    let rows = get_filtered_rows(text, all_rows);
                    tv.set_items(rows);
                }
            }
        })
        .on_submit(|siv, _| {
            siv.focus_name("vault_table")
                .expect("Focusing table failed");
        })
        .with_name("search_edit")
        .full_width();

    let ll = LinearLayout::horizontal()
        .child(TextView::new("🔍"))
        .child(filter_edit);

    PaddedView::lrtb(0, 0, 0, 1, ll)
}

fn get_filtered_rows(filter: &str, rows: &Vec<Row>) -> Vec<Row> {
    if filter.is_empty() {
        return rows.clone();
    }

    rows.iter()
        .filter_map(|r| {
            let score = fuzzywuzzy::fuzz::partial_token_sort_ratio(
                filter, &format!("{} {} {}", r.name, r.username, r.url), true, true);
            if score > 50 {
                Some((r, score))
            } else {
                None
            }
        })
        .sorted_by_key(|(_, score)| 100 - (*score))
        .map(|(r, _)| r)
        .cloned()
        .collect_vec()
}

fn vault_table_view(
    user_data: &mut UserData,
    enc_key: &EncryptionKey,
    mac_key: &MacKey,
) -> impl View {
    // Generate row items (with some decrypted data for all cipher items)
    // These are stored in user_data. Only the filter results are stored
    // as the table's rows.
    let rows = create_rows(user_data, enc_key, mac_key);
    user_data.vault_table_rows = Some(rows);

    let rows = user_data.vault_table_rows.clone().unwrap();

    let mut tv = TableView::new()
        .sorting_disabled()
        .column(VaultTableColumn::ItemType, "T", |c| c.width(1))
        .column(VaultTableColumn::Name, "Name", |c| c)
        .column(VaultTableColumn::Username, "Username", |c| c)
        .column(VaultTableColumn::IsInOrganization, "O", |c| c.width(1))
        .items(rows)
        .on_submit(|siv: &mut Cursive, _, index| {
            let sink = siv.cb_sink().clone();
            siv.call_on_name(
                "vault_table",
                move |t: &mut TableView<Row, VaultTableColumn>| {
                    show_item_details(sink, t.borrow_item(index).unwrap());
                },
            )
            .unwrap();
        });

    // Explicitly set the first row as selected. This is needed, because
    // for some reason the table view scrolls past and hides the first item
    // without this 
    tv.set_selected_row(0);

    tv.with_name("vault_table")
        .full_height()
}

fn create_rows(user_data: &mut UserData, enc_key: &EncryptionKey, mac_key: &MacKey) -> Vec<Row> {
    // Find all organization keys we will need
    let org_keys: HashMap<_, _> = user_data
        .vault_data
        .as_ref()
        .map(|ok| {
            ok.values()
                .filter_map(|i| i.organization_id.as_ref())
                .unique()
                .filter_map(|oid| {
                    user_data
                        .decrypt_organization_keys(oid)
                        .map(|key| (oid, key))
                        .ok()
                })
                .collect()
        })
        .unwrap_or(HashMap::new());

    user_data
        .vault_data
        .as_ref()
        .unwrap_or(&HashMap::new())
        .iter()
        .filter_map(|(id, ci)| {
            let (ec, mc) = if let Some(oid) = &ci.organization_id {
                let keys = org_keys.get(oid)?;
                (&keys.0, &keys.1)
            } else {
                (enc_key, mac_key)
            };
            Some(Row {
                id: id.clone(),
                name: ci.name.decrypt_to_string(ec, mc),
                url: String::new(),
                username: match &ci.data {
                    CipherData::Login(l) => &l.username,
                    _ => &Cipher::Empty,
                }
                .decrypt_to_string(ec, mc),
                item_type: match ci.data {
                    CipherData::Login(_) => "L",
                    CipherData::Card(_) => "C",
                    CipherData::Identity(_) => "I",
                    CipherData::SecureNote => "N",
                    _ => "",
                }
                .to_string(),
                is_in_organization: ci.organization_id.is_some(),
            })
        })
        .sorted()
        .collect_vec()
}

fn show_item_details(sink: cursive::CbSink, row: &Row) {
    let item_id = row.id.clone();
    sink.send(Box::new(move |siv: &mut Cursive| {
        let ud: &mut UserData = siv.user_data().unwrap();
        let dialog = item_detail_dialog(ud, &item_id);
        if let Some(d) = dialog {
            siv.add_layer(d);
        }
    }))
    .unwrap();
}

fn key_hint_view() -> impl View {
    fn hint_text(content: &str) -> impl View {
        PaddedView::new(
            Margins::lr(2, 2),
            TextView::new(content).style(Color::Light(BaseColor::Black)),
        )
    }

    LinearLayout::horizontal()
        .child(hint_text("</> Search"))
        .child(hint_text("<p> Copy password"))
        .child(hint_text("<u> Copy username"))
        .child(hint_text("<q> Quit"))
        .child(hint_text("<^s> Sync"))
        .full_width()
}

pub fn show_copy_notification(cursive: &mut Cursive, message: &'static str) {
    // Not using Dialog::info here so that a named view can be added to the dialog
    // The named view is used later to find the dialog
    cursive.add_layer(Dialog::info(message).with_name("copy_notification"));

    let cb = cursive.cb_sink().clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cb.send(Box::new(|siv| {
            let sc = siv.screen_mut();
            if let Some(LayerPosition::FromBack(l)) = sc.find_layer_from_name("copy_notification") {
                if l == sc.len() - 1 {
                    // If the dialog is the topmost layer, pop it
                    siv.pop_layer();
                }
            }
        }))
        .expect("Sending message failed");
    });
}
