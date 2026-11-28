//! Códigos TOTP (RFC 6238) para el segundo factor de autenticación.
//!
//! El secreto se introduce en base32 o como URI `otpauth://totp/…` (lo que
//! codifican los códigos QR de alta) y se guarda en el baúl, nunca en claro.

use anyhow::{Result, anyhow, bail};
use hmac::{Hmac, KeyInit, Mac};
use zeroize::Zeroizing;

use crate::i18n::tr;
use crate::trf;

/// Función resumen del HMAC con el que se calculan los códigos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotpAlgorithm {
    Sha1,
    Sha256,
    Sha512,
}

impl TotpAlgorithm {
    /// Nombre tal como va en las URI `otpauth://`.
    pub fn name(self) -> &'static str {
        match self {
            TotpAlgorithm::Sha1 => "SHA1",
            TotpAlgorithm::Sha256 => "SHA256",
            TotpAlgorithm::Sha512 => "SHA512",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TotpAlgorithm::Sha1 => "SHA-1",
            TotpAlgorithm::Sha256 => "SHA-256",
            TotpAlgorithm::Sha512 => "SHA-512",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text.to_ascii_uppercase().replace('-', "").as_str() {
            "SHA1" => Ok(TotpAlgorithm::Sha1),
            "SHA256" => Ok(TotpAlgorithm::Sha256),
            "SHA512" => Ok(TotpAlgorithm::Sha512),
            _ => bail!(
                "{}",
                trf!(
                    "Algoritmo TOTP no soportado: {text} (vale SHA1, SHA256 o SHA512)",
                    text
                )
            ),
        }
    }
}

/// Generador de códigos de un solo uso basados en la hora.
#[derive(Clone, PartialEq, Eq)]
pub struct Totp {
    secret: Zeroizing<Vec<u8>>,
    pub algorithm: TotpAlgorithm,
    /// Cifras de cada código (de 6 a 8).
    pub digits: u32,
    /// Segundos que vale cada código.
    pub period: u64,
}

// A mano, para que el secreto no acabe en un registro o en un `panic`.
impl std::fmt::Debug for Totp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Totp")
            .field("secret", &"…")
            .field("algorithm", &self.algorithm)
            .field("digits", &self.digits)
            .field("period", &self.period)
            .finish()
    }
}

const DEFAULT_DIGITS: u32 = 6;
const DEFAULT_PERIOD: u64 = 30;
/// Periodo más largo que se admite (un día).
const MAX_PERIOD: u64 = 86_400;
/// Tope del texto de un secreto o de una URI: lo que no cabe no es legítimo.
const MAX_INPUT: usize = 4096;

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Descodifica base32 (RFC 4648) sin distinguir mayúsculas e ignorando los
/// espacios, guiones y el relleno con los que se suele presentar un secreto.
fn base32_decode(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for c in text.chars() {
        if c.is_whitespace() || c == '-' || c == '=' {
            continue;
        }
        let upper = c.to_ascii_uppercase();
        let value = BASE32
            .iter()
            .position(|b| char::from(*b) == upper)
            .ok_or_else(|| {
                anyhow!(
                    "{}",
                    tr(
                        "El secreto TOTP no es base32 válido (solo letras de la A a la Z y \
                         cifras del 2 al 7)"
                    )
                )
            })?;
        buffer = (buffer << 5) | value as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in data {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(BASE32[((buffer >> bits) & 31) as usize]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(BASE32[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// Deshace el `%XX` de un valor de una URI (y el `+` de los espacios).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|b| char::from(*b).to_digit(16));
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (b'+', ..) => {
                out.push(b' ');
                i += 1;
            }
            (other, ..) => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Totp {
    /// Interpreta lo que escribe el usuario: un secreto en base32 (con los
    /// valores habituales: SHA-1, 6 cifras, 30 segundos) o una URI
    /// `otpauth://totp/…` con sus parámetros.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            bail!("{}", tr("Falta el secreto TOTP"));
        }
        if text.len() > MAX_INPUT {
            bail!("{}", tr("El secreto TOTP es demasiado largo"));
        }
        if text.len() >= 10 && text[..10].eq_ignore_ascii_case("otpauth://") {
            return Self::parse_uri(&text[10..]);
        }
        Self::new(
            base32_decode(text)?,
            TotpAlgorithm::Sha1,
            DEFAULT_DIGITS,
            DEFAULT_PERIOD,
        )
    }

    /// `rest` es la URI sin el `otpauth://` inicial.
    fn parse_uri(rest: &str) -> Result<Self> {
        let (kind, rest) = rest.split_once('/').unwrap_or((rest, ""));
        let kind = kind.split('?').next().unwrap_or_default();
        if kind.eq_ignore_ascii_case("hotp") {
            bail!(
                "{}",
                tr("Los códigos HOTP (por contador) no están soportados: solo TOTP (por hora)")
            );
        }
        if !kind.eq_ignore_ascii_case("totp") {
            bail!(
                "{}",
                tr("La URI no es de un código TOTP (otpauth://totp/…)")
            );
        }
        let query = rest.split_once('?').map(|(_, q)| q).unwrap_or_default();
        let query = query.split('#').next().unwrap_or_default();
        let mut secret = None;
        let mut algorithm = TotpAlgorithm::Sha1;
        let mut digits = DEFAULT_DIGITS;
        let mut period = DEFAULT_PERIOD;
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(value);
            match name.to_ascii_lowercase().as_str() {
                "secret" => secret = Some(base32_decode(&value)?),
                "algorithm" => algorithm = TotpAlgorithm::parse(&value)?,
                "digits" => {
                    digits = value.trim().parse().map_err(|_| {
                        anyhow!(
                            "{}",
                            trf!("Número de cifras TOTP no válido: {value}", value)
                        )
                    })?
                }
                "period" => {
                    period = value.trim().parse().map_err(|_| {
                        anyhow!("{}", trf!("Periodo TOTP no válido: {value}", value))
                    })?
                }
                // El emisor y lo demás no hacen falta para calcular.
                _ => {}
            }
        }
        let secret =
            secret.ok_or_else(|| anyhow!("{}", tr("La URI otpauth:// no trae el secreto")))?;
        Self::new(secret, algorithm, digits, period)
    }

    /// Generador con un secreto ya descodificado.
    pub fn new(
        secret: Vec<u8>,
        algorithm: TotpAlgorithm,
        digits: u32,
        period: u64,
    ) -> Result<Self> {
        let secret = Zeroizing::new(secret);
        if secret.is_empty() {
            bail!("{}", tr("Falta el secreto TOTP"));
        }
        if !(6..=8).contains(&digits) {
            bail!(
                "{}",
                trf!(
                    "Los códigos TOTP deben tener de 6 a 8 cifras, no {digits}",
                    digits
                )
            );
        }
        if !(1..=MAX_PERIOD).contains(&period) {
            bail!(
                "{}",
                trf!(
                    "El periodo TOTP debe estar entre 1 y {max} segundos",
                    max = MAX_PERIOD
                )
            );
        }
        Ok(Self {
            secret,
            algorithm,
            digits,
            period,
        })
    }

    /// Forma canónica, con todos los parámetros: es lo que se guarda en el
    /// baúl. Contiene el secreto.
    pub fn to_uri(&self) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "otpauth://totp/GodRemote?secret={}&algorithm={}&digits={}&period={}",
            base32_encode(&self.secret),
            self.algorithm.name(),
            self.digits,
            self.period
        ))
    }

    /// Número del periodo al que pertenece un instante (segundos desde 1970).
    pub fn step_at(&self, unix: u64) -> u64 {
        unix / self.period
    }

    /// Código de un periodo concreto (RFC 4226, con el contador de RFC 6238).
    pub fn code_for_step(&self, step: u64) -> String {
        let counter = step.to_be_bytes();
        // HMAC admite claves de cualquier longitud: no puede fallar.
        let digest: Vec<u8> = match self.algorithm {
            TotpAlgorithm::Sha1 => Hmac::<sha1::Sha1>::new_from_slice(&self.secret)
                .map(|mac| mac.chain_update(counter).finalize().into_bytes().to_vec()),
            TotpAlgorithm::Sha256 => Hmac::<sha2::Sha256>::new_from_slice(&self.secret)
                .map(|mac| mac.chain_update(counter).finalize().into_bytes().to_vec()),
            TotpAlgorithm::Sha512 => Hmac::<sha2::Sha512>::new_from_slice(&self.secret)
                .map(|mac| mac.chain_update(counter).finalize().into_bytes().to_vec()),
        }
        .unwrap_or_default();
        // Truncado dinámico: los 4 bits bajos del último byte dicen dónde
        // empiezan los 31 bits de los que sale el código.
        let offset = digest.last().map(|b| (b & 0x0f) as usize).unwrap_or(0);
        let word = digest
            .get(offset..offset + 4)
            .map(|b| u32::from_be_bytes([b[0] & 0x7f, b[1], b[2], b[3]]))
            .unwrap_or(0);
        let code = u64::from(word) % 10u64.pow(self.digits);
        format!("{code:0width$}", width = self.digits as usize)
    }

    /// Código que vale en un instante dado (segundos desde 1970).
    pub fn code_at(&self, unix: u64) -> String {
        self.code_for_step(self.step_at(unix))
    }

    /// Código que vale ahora.
    pub fn code(&self) -> String {
        self.code_at(unix_now())
    }

    /// Segundos que le quedan al código de ese instante (de 1 a `period`).
    pub fn seconds_left_at(&self, unix: u64) -> u64 {
        self.period - unix % self.period
    }

    pub fn seconds_left(&self) -> u64 {
        self.seconds_left_at(unix_now())
    }

    /// Identifica el secreto sin revelarlo (para recordar qué códigos se han
    /// usado ya).
    pub fn fingerprint(&self) -> [u8; 32] {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"godremote-totp");
        hasher.update(self.algorithm.name().as_bytes());
        hasher.update(self.digits.to_be_bytes());
        hasher.update(self.period.to_be_bytes());
        hasher.update(&self.secret[..]);
        hasher.finalize().into()
    }

    /// «SHA-1 · 6 cifras · 30 s», para enseñar con qué se calcula.
    pub fn describe(&self) -> String {
        trf!(
            "{algorithm} · {digits} cifras · cada {period} s",
            algorithm = self.algorithm.label(),
            digits = self.digits,
            period = self.period
        )
    }
}

/// El generador de un equipo, si tiene activado el relleno automático y su
/// secreto está en el baúl.
pub fn stored(session: &crate::model::Session, vault: &crate::vault::Vault) -> Option<Totp> {
    if !session.totp_autofill {
        return None;
    }
    Totp::parse(vault.get(&session.totp_key())?).ok()
}

/// Un código en grupos de tres o cuatro cifras, más fácil de leer: «123 456».
pub fn spaced(code: &str) -> String {
    let half = code.len().div_ceil(2);
    if code.len() < 6 || !code.is_char_boundary(half) {
        return code.to_string();
    }
    format!("{} {}", &code[..half], &code[half..])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectores del apéndice B de la RFC 6238 (códigos de 8 cifras).
    #[test]
    fn rfc_6238_vectors() {
        let secrets = [
            (TotpAlgorithm::Sha1, &b"12345678901234567890"[..]),
            (
                TotpAlgorithm::Sha256,
                &b"12345678901234567890123456789012"[..],
            ),
            (
                TotpAlgorithm::Sha512,
                &b"1234567890123456789012345678901234567890123456789012345678901234"[..],
            ),
        ];
        let vectors: [(u64, [&str; 3]); 6] = [
            (59, ["94287082", "46119246", "90693936"]),
            (1_111_111_109, ["07081804", "68084774", "25091201"]),
            (1_111_111_111, ["14050471", "67062674", "99943326"]),
            (1_234_567_890, ["89005924", "91819424", "93441116"]),
            (2_000_000_000, ["69279037", "90698825", "38618901"]),
            (20_000_000_000, ["65353130", "77737706", "47863826"]),
        ];
        for (time, expected) in vectors {
            for ((algorithm, secret), code) in secrets.iter().zip(expected) {
                let totp = Totp::new(secret.to_vec(), *algorithm, 8, 30).unwrap();
                assert_eq!(totp.code_at(time), code, "{algorithm:?} en {time}");
            }
        }
        // Con 6 cifras son las últimas del mismo número (RFC 4226).
        let six = Totp::new(secrets[0].1.to_vec(), TotpAlgorithm::Sha1, 6, 30).unwrap();
        assert_eq!(six.code_at(59), "287082");
        assert_eq!(six.code_at(1_111_111_109), "081804");
    }

    #[test]
    fn base32_secrets() {
        // «12345678901234567890» en base32, como lo da cualquier servicio.
        let plain = Totp::parse("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap();
        assert_eq!(
            (plain.algorithm, plain.digits, plain.period),
            (TotpAlgorithm::Sha1, 6, 30)
        );
        assert_eq!(plain.code_at(59), "287082");
        // En grupos, en minúsculas, con guiones y con relleno.
        for written in [
            "gezd gnbv gy3t qojq gezd gnbv gy3t qojq",
            "GEZD-GNBV-GY3T-QOJQ-GEZD-GNBV-GY3T-QOJQ",
            "  GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ====\n",
        ] {
            assert_eq!(Totp::parse(written).unwrap(), plain, "{written}");
        }
        // Ida y vuelta con longitudes que no son múltiplo de 5 bytes.
        for len in 1..=24usize {
            let data: Vec<u8> = (0..len as u8).map(|i| i.wrapping_mul(37) ^ 0xa5).collect();
            assert_eq!(base32_decode(&base32_encode(&data)).unwrap(), data);
        }
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");

        for bad in ["", "   ", "GEZD1GNB", "secreto!", "====", "0O0O0O0O"] {
            assert!(Totp::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Totp::parse(&"A".repeat(5000)).is_err());
    }

    #[test]
    fn otpauth_uris() {
        let totp = Totp::parse(
            "otpauth://totp/Empresa:ana%40empresa.es?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA\
             &issuer=Empresa&algorithm=SHA256&digits=8&period=30",
        )
        .unwrap();
        assert_eq!(
            (totp.algorithm, totp.digits, totp.period),
            (TotpAlgorithm::Sha256, 8, 30)
        );
        assert_eq!(totp.code_at(59), "46119246");
        assert_eq!(totp.describe(), "SHA-256 · 8 cifras · cada 30 s");

        // Sin parámetros valen los habituales; el orden y las mayúsculas dan igual.
        let short = Totp::parse("OTPAUTH://TOTP/x?Period=60&SECRET=gezdgnbvgy3tqojq").unwrap();
        assert_eq!(
            (short.algorithm, short.digits, short.period),
            (TotpAlgorithm::Sha1, 6, 60)
        );
        assert_eq!(short.seconds_left_at(61), 59);
        assert_eq!(short.seconds_left_at(120), 60);
        assert_eq!((short.step_at(59), short.step_at(60)), (0, 1));
        assert_eq!(short.code_at(60), short.code_at(119));
        assert_ne!(short.code_at(59), short.code_at(60));

        // La forma canónica conserva todo y se vuelve a leer igual.
        let uri = totp.to_uri();
        assert!(
            uri.contains("algorithm=SHA256&digits=8&period=30"),
            "{}",
            *uri
        );
        assert_eq!(Totp::parse(&uri).unwrap(), totp);
        assert_eq!(Totp::parse(&short.to_uri()).unwrap(), short);
        assert_ne!(totp.fingerprint(), short.fingerprint());

        for bad in [
            "otpauth://hotp/x?secret=GEZDGNBVGY3TQOJQ&counter=1",
            "otpauth://totp/x?issuer=Empresa",
            "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQ&digits=5",
            "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQ&digits=9",
            "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQ&period=0",
            "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQ&period=treinta",
            "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQ&algorithm=MD5",
            "otpauth://otra/x?secret=GEZDGNBVGY3TQOJQ",
            "otpauth://",
        ] {
            assert!(Totp::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let totp = Totp::parse("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap();
        let debug = format!("{totp:?}");
        assert!(
            !debug.contains("GEZD") && !debug.contains("49, 50"),
            "{debug}"
        );
        assert_eq!(spaced("123456"), "123 456");
        assert_eq!(spaced("1234567"), "1234 567");
        assert_eq!(spaced("12345678"), "1234 5678");
    }
}
