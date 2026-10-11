use anyhow::{Context as _, Result, ensure};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, Focusable, IntoElement, Render, Task, Window,
};
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use ui::{Button, Color, IconButton, IconName, Label, LabelSize, Tooltip, prelude::*};
use ui_input::InputField;
use workspace::ModalView;

const VAULT_KEY: &str = "momor://browser/payment-cards/v1";
type FillCard = Rc<dyn Fn(PaymentCard, &mut Window, &mut App) -> Task<Result<usize>>>;

#[derive(Clone, Serialize, Deserialize)]
pub struct PaymentCard {
    id: String,
    name: String,
    number: String,
    month: u32,
    year: u32,
}

impl PaymentCard {
    fn from_fields(name: &str, number: &str, expiration: &str) -> Result<Self> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 80,
            "Informe o nome no cartao (ate 80 caracteres)."
        );
        ensure!(
            number
                .chars()
                .all(|character| character.is_ascii_digit() || matches!(character, ' ' | '-')),
            "Numero do cartao invalido."
        );
        let number: String = number.chars().filter(char::is_ascii_digit).collect();
        ensure!(
            (12..=19).contains(&number.len()),
            "O numero deve conter de 12 a 19 digitos."
        );
        let (month, year) = expiration
            .trim()
            .split_once('/')
            .context("Use MM/AAAA para a validade.")?;
        let month: u32 = month.trim().parse().context("Mes invalido.")?;
        let year: u32 = year.trim().parse().context("Ano invalido.")?;
        ensure!(
            (1..=12).contains(&month) && (2000..=9999).contains(&year),
            "Validade invalida. Use MM/AAAA."
        );
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().to_owned(),
            number,
            month,
            year,
        })
    }

    fn label(&self) -> String {
        let ending = self
            .number
            .get(self.number.len().saturating_sub(4)..)
            .unwrap_or_default();
        format!("{} - **** {}", self.name, ending)
    }

    pub fn fill_script(&self, origin: &str) -> Result<String> {
        let url = url::Url::parse(origin)?;
        ensure!(
            url.scheme() == "https" && url.host_str().is_some(),
            "Cartoes so podem ser preenchidos em sites HTTPS."
        );
        Ok(format!(
            "({})({}, {})",
            include_str!("browser_payment_fill.js"),
            serde_json::to_string(&url.origin().ascii_serialization())?,
            serde_json::to_string(self)?
        ))
    }
}

pub struct BrowserPayments {
    name: Entity<InputField>,
    number: Entity<InputField>,
    expiration: Entity<InputField>,
    cards: Vec<PaymentCard>,
    editing: Option<String>,
    busy: bool,
    vault_loaded: bool,
    error: Option<String>,
    fill: FillCard,
}

impl EventEmitter<DismissEvent> for BrowserPayments {}
impl ModalView for BrowserPayments {}
impl Focusable for BrowserPayments {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }
}

impl BrowserPayments {
    pub fn new(
        fill: impl Fn(PaymentCard, &mut Window, &mut App) -> Task<Result<usize>> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputField::new(window, cx, "Nome no cartao")
                .label("Titular")
                .label_min_width(px(78.))
        });
        let number = cx.new(|cx| {
            InputField::new(window, cx, "Numero do cartao")
                .label("Numero")
                .label_min_width(px(78.))
                .masked(true)
        });
        let expiration = cx.new(|cx| {
            InputField::new(window, cx, "MM/AAAA")
                .label("Validade")
                .label_min_width(px(78.))
        });
        let read = cx.read_credentials(VAULT_KEY);
        cx.spawn(async move |this, cx| {
            let result = read.await.and_then(|saved| {
                saved
                    .map(|(_, bytes)| {
                        serde_json::from_slice::<Vec<PaymentCard>>(&bytes)
                            .context("Nao foi possivel ler os cartoes salvos.")
                    })
                    .transpose()
                    .map(Option::unwrap_or_default)
            });
            if let Err(error) = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(cards) => {
                        this.cards = cards;
                        this.vault_loaded = true;
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            }) {
                log::error!("Payment vault UI unavailable: {error:#}");
            }
        })
        .detach();
        Self {
            name,
            number,
            expiration,
            cards: Vec::new(),
            editing: None,
            busy: true,
            vault_loaded: false,
            error: None,
            fill: Rc::new(fill),
        }
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for field in [&self.name, &self.number, &self.expiration] {
            field.update(cx, |field, cx| field.set_text("", window, cx));
        }
        self.editing = None;
        self.error = None;
        cx.notify();
    }

    fn persist(&mut self, cards: Vec<PaymentCard>, window: &mut Window, cx: &mut Context<Self>) {
        // A failed vault read must never turn into overwriting existing credentials.
        if self.busy || !self.vault_loaded {
            return;
        }
        let bytes = match serde_json::to_vec(&cards) {
            Ok(bytes) if bytes.len() <= 2000 => bytes,
            Ok(_) => {
                self.error = Some("Limite de cartoes no cofre atingido.".into());
                cx.notify();
                return;
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        // Credentials go through the platform vault, never the browser history or SQLite settings.
        let write = cx.write_credentials(VAULT_KEY, "Momor", &bytes);
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = write.await;
            if let Err(error) = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.cards = cards;
                        this.reset(window, cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Nao foi possivel salvar no cofre: {error}"))
                    }
                }
                cx.notify();
            }) {
                log::error!("Payment vault UI unavailable: {error:#}");
            }
        })
        .detach();
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.vault_loaded {
            return;
        }
        let card = PaymentCard::from_fields(
            &self.name.read(cx).text(cx),
            &self.number.read(cx).text(cx),
            &self.expiration.read(cx).text(cx),
        );
        match card {
            Ok(mut card) => {
                let mut cards = self.cards.clone();
                if let Some(id) = &self.editing {
                    card.id = id.clone();
                    cards.retain(|card| &card.id != id);
                }
                cards.push(card);
                self.persist(cards, window, cx);
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn apply(&mut self, card: PaymentCard, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let fill = (self.fill)(card, window, cx);
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = fill.await;
            if let Err(error) = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(count) if count > 0 => cx.emit(DismissEvent),
                    Ok(_) => this.error = Some("Nenhum campo de cartao acessivel nesta pagina. Campos em iframes de pagamento externos devem ser preenchidos no proprio site.".into()),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            }) { log::error!("Payment fill UI unavailable: {error:#}"); }
        }).detach();
        cx.notify();
    }
}

impl Render for BrowserPayments {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ui::begin_popover(&cx.entity(), window, cx);
        let rows = self
            .cards
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, card)| {
                let fill = card.clone();
                let edit = card.clone();
                let id = card.id.clone();
                h_flex()
                    .w_full()
                    .gap_2()
                    .py_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(card.label()).truncate()),
                    )
                    .child(
                        Button::new(("fill-card", index), "Preencher")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.apply(fill.clone(), window, cx)
                            })),
                    )
                    .child(
                        IconButton::new(("edit-card", index), IconName::Pencil)
                            .disabled(self.busy)
                            .tooltip(Tooltip::text("Editar cartao"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.name
                                    .update(cx, |field, cx| field.set_text(&edit.name, window, cx));
                                this.number.update(cx, |field, cx| {
                                    field.set_text(&edit.number, window, cx)
                                });
                                this.expiration.update(cx, |field, cx| {
                                    field.set_text(
                                        &format!("{:02}/{}", edit.month, edit.year),
                                        window,
                                        cx,
                                    )
                                });
                                this.editing = Some(edit.id.clone());
                                this.error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        IconButton::new(("delete-card", index), IconName::Trash)
                            .disabled(self.busy)
                            .tooltip(Tooltip::text("Remover cartao"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let cards = this
                                    .cards
                                    .iter()
                                    .filter(|card| card.id != id)
                                    .cloned()
                                    .collect();
                                this.persist(cards, window, cx);
                            })),
                    )
            });
        let content = v_flex()
            .key_context("Modal")
            .track_focus(&self.focus_handle(cx))
            .w(px(440.))
            .max_w(window.viewport_size().width - px(24.))
            .elevation_3(cx)
            .rounded_sm()
            .p_4()
            .gap_3()
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(Label::new("Cartoes salvos").size(LabelSize::Large))
                    .child(
                        IconButton::new("close-payment-vault", IconName::Close)
                            .tooltip(Tooltip::text("Fechar"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                v_flex()
                    .id("saved-payment-cards")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(self.name.clone())
            .child(self.number.clone())
            .child(self.expiration.clone())
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("save-payment-card", "Salvar cartao")
                            .disabled(self.busy || !self.vault_loaded)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    )
                    .child(
                        IconButton::new("new-payment-card", IconName::Plus)
                            .disabled(self.busy || !self.vault_loaded)
                            .tooltip(Tooltip::text("Novo cartao"))
                            .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
                    ),
            );
        ui::native_surface_overlay(cx.entity_id(), content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payment_profiles_mask_numbers_and_never_store_cvv() -> Result<()> {
        let card = PaymentCard::from_fields("Teste", "4111 1111 1111 1111", "12/2099")?;
        assert_eq!(card.label(), "Teste - **** 1111");
        let stored = serde_json::to_value(&card)?;
        assert!(stored.get("cvv").is_none());
        assert!(stored.get("cvc").is_none());
        assert!(card.fill_script("http://example.com").is_err());
        assert!(card.fill_script("https://example.com").is_ok());
        assert!(PaymentCard::from_fields("Teste", "bad-number", "12/2099").is_err());
        assert!(PaymentCard::from_fields("Teste", "4111111111111111", "13/2099").is_err());
        Ok(())
    }
}
