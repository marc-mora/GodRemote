//! Piezas del editor de equipos para la autenticación SSH: el certificado
//! que acompaña a la clave privada y el segundo factor TOTP.

use std::time::{Duration, Instant};

use egui::RichText;

use crate::i18n::tr;
use crate::keys::{self, CertInfo, CertValidity};
use crate::theme::{error_color, ok_color, warn_color};
use crate::totp::Totp;
use crate::trf;
use crate::vault::Vault;

/// Texto y color con los que se resume la validez de un certificado.
pub fn validity_label(info: &CertInfo) -> (String, egui::Color32) {
    let text = info.validity_text();
    match info.validity() {
        CertValidity::Valid => (text, ok_color()),
        CertValidity::NotYetValid => (format!("⚠ {text}"), warn_color()),
        CertValidity::Expired => (format!("⚠ {text}"), error_color()),
    }
}

/// La misma validez como frase: «El certificado caducó el…».
fn validity_sentence(info: &CertInfo) -> (String, egui::Color32) {
    let text = info.validity_text();
    match info.validity() {
        CertValidity::Valid => (trf!("Certificado {validity}", validity = text), ok_color()),
        CertValidity::NotYetValid => (
            trf!("⚠ El certificado {validity}", validity = text),
            warn_color(),
        ),
        CertValidity::Expired => (
            trf!("⚠ El certificado {validity}", validity = text),
            error_color(),
        ),
    }
}

/// Todos los datos de un certificado, uno por línea (para el texto de ayuda).
pub fn cert_details(info: &CertInfo) -> String {
    trf!(
        "Fichero: {file}\nIdentidad: {identity}\nNúmero de serie: {serial}\nTipo: {kind}\n\
         Principales: {principals}\nValidez: {validity}\n\
         Autoridad (CA): {ca_algorithm} {ca_fingerprint}\nClave certificada: {key}",
        file = info.path.display(),
        identity = info.key_id,
        serial = info.serial,
        kind = if info.host {
            tr("de host")
        } else {
            tr("de usuario")
        },
        principals = info.principals_text(),
        validity = info.validity_text(),
        ca_algorithm = info.ca_algorithm,
        ca_fingerprint = info.ca_fingerprint,
        key = info.key_fingerprint,
    )
}

/// El algoritmo de una clave para una lista. Las de las llaves de seguridad
/// FIDO2 se presentan como tales en vez de con su nombre técnico.
pub fn algorithm_label(ui: &mut egui::Ui, algorithm: &str, security_key: bool) {
    match keys::security_key_label(algorithm).filter(|_| security_key) {
        Some(short) => {
            ui.label(RichText::new(tr("🔐 llave de seguridad")).color(ok_color()))
                .on_hover_text(trf!(
                    "Clave de una llave de seguridad FIDO2 ({algorithm}). Quien firma es la \
                     propia llave: tiene que estar conectada y cargada en el agente SSH \
                     (ssh-add), y habrá que tocarla al conectar",
                    algorithm
                ));
            ui.label(short);
        }
        None => {
            ui.label(algorithm);
        }
    }
}

/// Lo que se sabe del certificado que se usará con la clave del editor.
#[derive(Default)]
enum CertState {
    /// No hay certificado: se usará la clave sola.
    #[default]
    None,
    Found(CertInfo),
    Problem(String),
}

/// Campo «Certificado» del editor, con un resumen del que se va a usar: el
/// indicado o el que haya junto a la clave.
#[derive(Default)]
pub struct CertField {
    /// Rutas (clave, certificado) para las que vale lo calculado.
    seen: (String, String),
    checked: Option<Instant>,
    state: CertState,
    /// La clave elegida es de una llave de seguridad FIDO2.
    security_key: bool,
}

/// Cada cuánto se vuelve a mirar el disco (un certificado recién renovado
/// debe verse sin cerrar el editor).
const RECHECK: Duration = Duration::from_secs(2);

impl CertField {
    fn refresh(&mut self, key_path: &str, cert_path: &str) {
        let current = (key_path.to_string(), cert_path.to_string());
        let fresh = self.checked.is_some_and(|at| at.elapsed() < RECHECK);
        if self.seen == current && fresh {
            return;
        }
        self.seen = current;
        self.checked = Some(Instant::now());
        let key = keys::inspect(&crate::paths::expand_tilde(key_path));
        self.security_key = key.as_ref().is_some_and(|k| k.security_key);
        self.state = Self::look(key_path.trim(), cert_path.trim(), key.as_ref());
    }

    fn look(key_path: &str, cert_path: &str, key_info: Option<&keys::KeyInfo>) -> CertState {
        let explicit = !cert_path.is_empty();
        if !explicit && key_path.is_empty() {
            return CertState::None;
        }
        let key = crate::paths::expand_tilde(key_path);
        let path = if explicit {
            crate::paths::expand_tilde(cert_path)
        } else {
            keys::cert_path_for(&key)
        };
        if !explicit && !path.exists() {
            return CertState::None;
        }
        let info = match keys::read_cert(&path) {
            Ok((_, info)) => info,
            Err(e) => return CertState::Problem(format!("{e:#}")),
        };
        // Si la clave se puede leer, se comprueba que el certificado es suyo.
        let key_fingerprint = key_info.map(|k| k.fingerprint.as_str());
        if key_fingerprint.is_some_and(|f| !f.is_empty() && f != info.key_fingerprint) {
            return CertState::Problem(tr("Ese certificado no corresponde a esta clave").into());
        }
        if info.host {
            return CertState::Problem(tr("Es un certificado de host, no de usuario").into());
        }
        CertState::Found(info)
    }

    /// Dibuja el campo y el resumen. `width` es el ancho de los campos del
    /// formulario.
    pub fn ui(&mut self, ui: &mut egui::Ui, key_path: &str, cert_path: &mut String, width: f32) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(cert_path)
                    .hint_text(tr("certificado: <clave>-cert.pub si lo hay"))
                    .desired_width(width - 44.0),
            )
            .on_hover_text(tr(
                "Certificado OpenSSH de la clave, firmado por una autoridad en la que confía \
                 el servidor. Vacío, se usa el fichero «<clave>-cert.pub» si existe",
            ));
            if ui
                .button("📂")
                .on_hover_text(tr("Elegir el fichero del certificado"))
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title(tr("Certificado OpenSSH (…-cert.pub)"))
                    .pick_file()
            {
                *cert_path = path.display().to_string();
            }
        });
        self.refresh(key_path, cert_path);
        if self.security_key {
            ui.scope(|ui| {
                ui.set_max_width(width);
                ui.weak(tr(
                    "🔐 Llave de seguridad: firma la propia llave a través del agente SSH, \
                     donde tiene que estar cargada (ssh-add).",
                ));
            });
        }
        match &self.state {
            CertState::None => {}
            CertState::Problem(problem) => {
                ui.colored_label(warn_color(), format!("⚠ {problem}"));
            }
            CertState::Found(info) => {
                let (validity, color) = validity_sentence(info);
                ui.scope(|ui| {
                    ui.set_max_width(width);
                    ui.add(
                        egui::Label::new(
                            RichText::new(trf!(
                                "📜 Certificado «{id}» para {principals}",
                                id = info.key_id,
                                principals = info.principals_text()
                            ))
                            .weak(),
                        )
                        .truncate(),
                    )
                    .on_hover_text(cert_details(info));
                    ui.colored_label(color, validity)
                        .on_hover_text(cert_details(info));
                });
                // La cuenta de la validez cambia sola con el tiempo.
                ui.ctx().request_repaint_after(RECHECK);
            }
        }
    }
}

/// Campo «Segundo factor» del editor: el secreto TOTP del equipo (que va al
/// baúl), el código que vale ahora con su cuenta atrás y la opción de
/// rellenarlo solo al conectar.
#[derive(Default)]
pub struct TotpField {
    /// Secreto nuevo que se está escribiendo (base32 o `otpauth://…`).
    input: String,
    /// Olvidar el secreto guardado.
    forget: bool,
    /// Lo escrito ya era un secreto válido en el fotograma anterior.
    was_valid: bool,
}

/// El código que vale ahora, lo que le queda y un botón para copiarlo.
fn code_line(ui: &mut egui::Ui, label: &str, totp: &Totp) {
    let code = totp.code();
    let left = totp.seconds_left();
    ui.horizontal(|ui| {
        ui.label(label);
        ui.label(
            RichText::new(crate::totp::spaced(&code))
                .monospace()
                .strong()
                .size(15.0),
        );
        ui.add(
            egui::ProgressBar::new(left as f32 / totp.period as f32)
                .desired_width(46.0)
                .desired_height(6.0),
        );
        // Los últimos segundos, en color de aviso: puede no dar tiempo.
        let text = RichText::new(format!("{left} s"));
        ui.label(if left <= 5 {
            text.color(warn_color())
        } else {
            text.weak()
        });
        if ui
            .small_button(tr("Copiar"))
            .on_hover_text(tr("Copia el código al portapapeles"))
            .clicked()
        {
            ui.ctx().copy_text(code);
        }
    });
    ui.ctx().request_repaint_after(Duration::from_millis(250));
}

impl TotpField {
    /// Dibuja el campo. `key` es la entrada del baúl con el secreto del
    /// equipo y `autofill`, su opción de rellenar el código al conectar.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        vault: Option<&Vault>,
        key: &str,
        autofill: &mut bool,
        width: f32,
    ) {
        ui.vertical(|ui| {
            let stored = vault
                .and_then(|v| v.get(key))
                .and_then(|uri| Totp::parse(uri).ok());
            let keeps_stored = stored.is_some() && !self.forget;
            let hint = if keeps_stored {
                tr("guardado en el baúl (escribe para cambiarlo)")
            } else {
                tr("secreto TOTP: base32 u otpauth://… (opcional)")
            };
            ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .password(true)
                    .hint_text(hint)
                    .desired_width(width),
            )
            .on_hover_text(tr(
                "El secreto que da el servidor al activar la verificación en dos pasos: el \
                 texto que acompaña al código QR, o la dirección otpauth:// que este contiene. \
                 Se guarda cifrado en el baúl, nunca en claro",
            ));
            let typed = (!self.input.trim().is_empty()).then(|| Totp::parse(&self.input));
            let valid = matches!(typed, Some(Ok(_)));
            // Al escribir un secreto válido se activa el relleno automático
            // (una vez: después manda la casilla).
            if valid && !self.was_valid {
                *autofill = true;
            }
            self.was_valid = valid;
            match &typed {
                Some(Err(e)) => {
                    ui.colored_label(error_color(), format!("{e:#}"));
                }
                Some(Ok(totp)) => {
                    code_line(ui, tr("Código actual:"), totp);
                    ui.weak(totp.describe());
                }
                None => {
                    if let Some(totp) = stored.as_ref().filter(|_| keeps_stored) {
                        code_line(ui, tr("Código actual:"), totp);
                    }
                }
            }
            if vault.is_none() {
                ui.weak(tr(
                    "Baúl bloqueado: el secreto TOTP no se puede guardar ni usar.",
                ));
            } else if valid || keeps_stored {
                ui.checkbox(autofill, tr("Rellenar el código solo al conectar"))
                    .on_hover_text(tr(
                        "Cuando el servidor pida un código de verificación (por \
                         teclado-interactivo), se contesta con el de este secreto. Con el baúl \
                         bloqueado, o si el servidor no acepta el código, se pregunta como siempre",
                    ));
            }
            if stored.is_some() {
                ui.checkbox(&mut self.forget, tr("Olvidar el secreto TOTP guardado"));
            }
        });
    }

    /// Lleva al baúl lo que haya cambiado y deja coherente la opción del
    /// equipo. Un secreto escrito con el baúl bloqueado es un error: no se
    /// guarda en claro ni se pierde sin avisar.
    pub fn commit(
        &mut self,
        session: &mut crate::model::Session,
        mut vault: Option<&mut Vault>,
    ) -> Result<(), String> {
        let key = session.totp_key();
        let failed = |e: anyhow::Error| {
            trf!(
                "No se pudo actualizar el baúl: {error}",
                error = format!("{e:#}")
            )
        };
        if !session.protocol.is_ssh() {
            // El segundo factor solo existe en las conexiones SSH.
            session.totp_autofill = false;
            if let Some(vault) = vault.as_deref_mut() {
                vault.remove(&key).map_err(failed)?;
            }
            return Ok(());
        }
        if !self.input.trim().is_empty() {
            let totp = Totp::parse(&self.input).map_err(|e| format!("{e:#}"))?;
            let Some(vault) = vault.as_deref_mut() else {
                return Err(tr(
                    "Desbloquea el baúl para guardar el secreto TOTP: no se guarda en claro",
                )
                .into());
            };
            vault.set(&key, &totp.to_uri()).map_err(failed)?;
            self.input.clear();
            self.forget = false;
        } else if self.forget
            && let Some(vault) = vault.as_deref_mut()
        {
            vault.remove(&key).map_err(failed)?;
            self.forget = false;
        }
        // Sin secreto guardado no hay con qué rellenar.
        if vault.is_some_and(|v| !v.contains(&key)) {
            session.totp_autofill = false;
        }
        Ok(())
    }
}

/// Elección del agente SSH en los ajustes. Solo se ofrece en Windows, que es
/// donde conviven dos (en los demás sistemas manda `SSH_AUTH_SOCK`).
pub fn agent_choice_ui(ui: &mut egui::Ui, choice: &mut crate::agent::AgentChoice) {
    if !cfg!(windows) {
        return;
    }
    ui.horizontal(|ui| {
        ui.label(tr("Agente SSH"));
        egui::ComboBox::from_id_salt("agent_choice")
            .selected_text(tr(choice.label()))
            .width(290.0)
            .show_ui(ui, |ui| {
                for option in crate::agent::AgentChoice::ALL {
                    ui.selectable_value(choice, option, tr(option.label()));
                }
            });
    })
    .response
    .on_hover_text(tr(
        "Con qué agente se autentican los equipos con «Agente SSH» y cuál se reenvía con \
         «Reenviar el agente SSH»: el servicio ssh-agent de Windows (OpenSSH) o Pageant, el de \
         PuTTY. En automático se usa el de OpenSSH y, si no está en marcha o no tiene claves, \
         Pageant",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Protocol, Session};

    #[test]
    fn totp_secret_goes_to_the_vault_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let mut vault = Vault::create(&path, "maestra").unwrap();
        let mut session = Session {
            totp_autofill: true,
            ..Default::default()
        };
        let key = session.totp_key();

        // Con el baúl bloqueado no se guarda (ni en claro ni de ninguna forma).
        let mut field = TotpField {
            input: "GEZD GNBV GY3T QOJQ".into(),
            ..Default::default()
        };
        let error = field.commit(&mut session, None).unwrap_err();
        assert!(error.contains("Desbloquea el baúl"), "{error}");
        assert!(session.totp_autofill);

        // Con el baúl abierto se guarda en su forma canónica, cifrado.
        field.commit(&mut session, Some(&mut vault)).unwrap();
        assert!(field.input.is_empty());
        let stored = vault.get(&key).unwrap().to_string();
        assert!(stored.starts_with("otpauth://totp/"), "{stored}");
        assert!(stored.contains("secret=GEZDGNBVGY3TQOJQ&algorithm=SHA1&digits=6&period=30"));
        assert!(session.totp_autofill);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("GEZDGNBVGY3TQOJQ") && !raw.contains("otpauth"));
        assert_eq!(
            crate::totp::stored(&session, &vault).map(|t| t.code_at(59)),
            Some(Totp::parse("GEZDGNBVGY3TQOJQ").unwrap().code_at(59))
        );

        // Algo que no es un secreto no toca lo guardado.
        field.input = "esto no es base32!".into();
        assert!(field.commit(&mut session, Some(&mut vault)).is_err());
        assert_eq!(vault.get(&key), Some(stored.as_str()));

        // Sin la opción marcada, el secreto se conserva pero no se usa.
        field.input.clear();
        session.totp_autofill = false;
        field.commit(&mut session, Some(&mut vault)).unwrap();
        assert!(vault.contains(&key));
        assert!(crate::totp::stored(&session, &vault).is_none());

        // Olvidarlo lo borra y desactiva el relleno.
        session.totp_autofill = true;
        field.forget = true;
        field.commit(&mut session, Some(&mut vault)).unwrap();
        assert!(!vault.contains(&key) && !session.totp_autofill);

        // Un equipo que deja de ser SSH no conserva su secreto.
        field.input = "GEZDGNBVGY3TQOJQ".into();
        session.totp_autofill = true;
        field.commit(&mut session, Some(&mut vault)).unwrap();
        assert!(vault.contains(&key));
        session.protocol = Protocol::Vnc;
        field.input.clear();
        field.commit(&mut session, Some(&mut vault)).unwrap();
        assert!(!vault.contains(&key) && !session.totp_autofill);
    }
}
