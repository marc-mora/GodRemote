//! Portapapeles compartido (solo texto) sobre el canal CLIPRDR de IronRDP.
//!
//! El canal vive dentro de la sesión de IronRDP y avisa de lo que ocurre con
//! [`ClipSignal`]; el bucle de la sesión se lo pasa a [`Clipboard`], que
//! decide qué contestar. El texto remoto se trae en cuanto se copia allí
//! (así la interfaz lo tiene ya en su portapapeles) y el local se sirve
//! cuando el remoto lo pide al pegar.

use ironrdp_cliprdr::CliprdrClient;
use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
    OwnedFormatDataResponse,
};
use ironrdp_session::{ActiveStage, ActiveStageOutput};
use tokio::sync::mpsc;

/// Tope del texto de portapapeles aceptado del servidor (en bytes UTF-16).
const MAX_CLIPBOARD_BYTES: usize = 32 * 1024 * 1024;

/// Avisos del canal de portapapeles (que vive dentro de IronRDP) al bucle de
/// la sesión.
#[derive(Debug)]
pub(super) enum ClipSignal {
    /// El servidor espera nuestra lista de formatos (inicialización).
    Advertise,
    /// El remoto ha copiado texto.
    RemoteCopy,
    /// El remoto pide nuestros datos en este formato.
    DataRequest(ClipboardFormatId),
    /// Texto recibido del remoto.
    Received(String),
    /// El remoto pide un fichero (no se ofrecen): se contesta con un error.
    FileRequest(u32),
}

/// Extremo del canal CLIPRDR: solo reenvía lo que pasa al bucle de la sesión.
#[derive(Debug)]
pub(super) struct TextClipboard {
    tx: mpsc::UnboundedSender<ClipSignal>,
}

ironrdp_core::impl_as_any!(TextClipboard);

impl TextClipboard {
    pub(super) fn new(tx: mpsc::UnboundedSender<ClipSignal>) -> Self {
        Self { tx }
    }

    fn send(&self, signal: ClipSignal) {
        let _ = self.tx.send(signal);
    }
}

impl CliprdrBackend for TextClipboard {
    fn temporary_directory(&self) -> &str {
        // Solo lo usan las transferencias de ficheros, que no se ofrecen.
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        self.send(ClipSignal::Advertise);
    }

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        if available_formats
            .iter()
            .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
        {
            self.send(ClipSignal::RemoteCopy);
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.send(ClipSignal::DataRequest(request.format));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() || response.data().len() > MAX_CLIPBOARD_BYTES {
            return;
        }
        if let Ok(text) = response.to_unicode_string() {
            self.send(ClipSignal::Received(text));
        }
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        self.send(ClipSignal::FileRequest(request.stream_id));
    }

    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _: LockDataId) {}

    fn on_unlock(&mut self, _: LockDataId) {}
}

/// Mensajes que hay que enviar por el canal de portapapeles.
pub(super) type ClipMessages = ironrdp_cliprdr::CliprdrSvcMessages<ironrdp_cliprdr::Client>;

/// Efecto de un aviso del canal de portapapeles.
#[derive(Default)]
pub(super) struct ClipReply {
    /// Lo que hay que enviar al servidor.
    pub(super) messages: Option<ClipMessages>,
    /// Texto nuevo del portapapeles remoto, para la interfaz.
    pub(super) text: Option<String>,
}

impl ClipReply {
    /// Los fallos del canal se ignoran: el portapapeles nunca debe tumbar la
    /// sesión.
    fn send(result: ironrdp_pdu::PduResult<ClipMessages>) -> Self {
        Self {
            messages: result.ok(),
            text: None,
        }
    }
}

/// Estado del portapapeles compartido (solo texto).
pub(super) struct Clipboard {
    pub(super) enabled: bool,
    /// Texto del portapapeles local (lo que se sirve al remoto).
    local: Option<String>,
    /// Último texto recibido del remoto: si la interfaz nos lo devuelve como
    /// «texto local» es un eco y no se vuelve a anunciar.
    remote: Option<String>,
}

impl Clipboard {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            local: None,
            remote: None,
        }
    }

    /// Anota el texto local sin anunciarlo (aún no hay sesión): se ofrecerá
    /// al inicializarse el canal.
    pub(super) fn remember_local(&mut self, text: String) {
        if self.enabled {
            self.local = Some(text);
        }
    }

    fn formats(&self) -> Vec<ClipboardFormat> {
        if self.local.is_some() {
            vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]
        } else {
            vec![]
        }
    }

    /// El portapapeles local ha cambiado: se anuncia al remoto, que pedirá
    /// el texto cuando alguien pegue allí.
    pub(super) fn local_copy(
        &mut self,
        text: String,
        cliprdr: &mut CliprdrClient,
    ) -> Option<ClipMessages> {
        if !self.enabled || self.remote.as_deref() == Some(text.as_str()) {
            return None;
        }
        self.local = Some(text);
        self.remote = None;
        // Antes de estar listo, la lista se envía cuando la pide el servidor.
        if !cliprdr.is_ready() {
            return None;
        }
        cliprdr.initiate_copy(&self.formats()).ok()
    }

    /// Atiende un aviso del canal.
    pub(super) fn on_signal(
        &mut self,
        signal: ClipSignal,
        cliprdr: &mut CliprdrClient,
    ) -> ClipReply {
        match signal {
            ClipSignal::Advertise => ClipReply::send(cliprdr.initiate_copy(&self.formats())),
            // Se trae ya el texto: así la interfaz lo tiene en su portapapeles
            // en cuanto se copia en el remoto.
            ClipSignal::RemoteCopy => {
                ClipReply::send(cliprdr.initiate_paste(ClipboardFormatId::CF_UNICODETEXT))
            }
            ClipSignal::DataRequest(format) => {
                let response = match (&self.local, format) {
                    (Some(text), ClipboardFormatId::CF_UNICODETEXT) => {
                        OwnedFormatDataResponse::new_unicode_string(&to_remote_newlines(text))
                    }
                    _ => OwnedFormatDataResponse::new_error(),
                };
                ClipReply::send(cliprdr.submit_format_data(response))
            }
            ClipSignal::Received(text) => {
                let text = to_local_newlines(text);
                // Si es lo que ya se tenía (de un lado o del otro) no hay
                // nada nuevo que contar.
                let known = [&self.local, &self.remote]
                    .iter()
                    .any(|held| held.as_deref() == Some(text.as_str()));
                if known {
                    return ClipReply::default();
                }
                self.remote = Some(text.clone());
                ClipReply {
                    messages: None,
                    text: Some(text),
                }
            }
            ClipSignal::FileRequest(stream_id) => ClipReply::send(
                cliprdr.submit_file_contents(FileContentsResponse::new_error(stream_id)),
            ),
        }
    }
}

/// Aplica una operación al canal de portapapeles de la sesión y prepara el
/// envío de lo que produzca.
pub(super) fn with_cliprdr(
    stage: &mut ActiveStage,
    operation: impl FnOnce(&mut CliprdrClient) -> Option<ClipMessages>,
) -> Vec<ActiveStageOutput> {
    let Some(messages) = stage
        .get_svc_processor_mut::<CliprdrClient>()
        .and_then(operation)
    else {
        return Vec::new();
    };
    match stage.process_svc_processor_messages(messages) {
        Ok(data) if !data.is_empty() => vec![ActiveStageOutput::ResponseFrame(data)],
        _ => Vec::new(),
    }
}

/// Windows espera los saltos de línea como CR+LF.
fn to_remote_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous = '\0';
    for c in text.chars() {
        if c == '\n' && previous != '\r' {
            out.push('\r');
        }
        out.push(c);
        previous = c;
    }
    out
}

/// Fuera de Windows, los CR+LF del remoto pasan a ser saltos sencillos.
fn to_local_newlines(text: String) -> String {
    if cfg!(windows) {
        text
    } else {
        text.replace("\r\n", "\n")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ironrdp_cliprdr::CliprdrServer;
    use ironrdp_svc::{SvcMessage, SvcProcessor as _};

    use super::*;

    /// Lo que ve el extremo «servidor» del canal de portapapeles.
    #[derive(Debug, Default)]
    struct ServerLog {
        /// Formatos anunciados por el cliente en cada copia.
        copies: Vec<Vec<ClipboardFormatId>>,
        /// Formatos que el cliente ha pedido.
        requests: Vec<ClipboardFormatId>,
        /// Datos recibidos del cliente (`None`: respondió con un error).
        data: Vec<Option<String>>,
    }

    #[derive(Debug)]
    struct ServerSide(Arc<Mutex<ServerLog>>);

    ironrdp_core::impl_as_any!(ServerSide);

    impl CliprdrBackend for ServerSide {
        fn temporary_directory(&self) -> &str {
            ""
        }

        fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
            ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
        }

        fn on_ready(&mut self) {}

        fn on_request_format_list(&mut self) {}

        fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

        fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
            let formats = available_formats.iter().map(ClipboardFormat::id).collect();
            self.0.lock().unwrap().copies.push(formats);
        }

        fn on_format_data_request(&mut self, request: FormatDataRequest) {
            self.0.lock().unwrap().requests.push(request.format);
        }

        fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
            let text = (!response.is_error()).then(|| response.to_unicode_string().unwrap());
            self.0.lock().unwrap().data.push(text);
        }

        fn on_file_contents_request(&mut self, _: FileContentsRequest) {}

        fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}

        fn on_lock(&mut self, _: LockDataId) {}

        fn on_unlock(&mut self, _: LockDataId) {}
    }

    /// Un canal de portapapeles completo en memoria: nuestro extremo cliente
    /// (con su estado) frente al extremo servidor de IronRDP.
    struct Wire {
        client: CliprdrClient,
        server: CliprdrServer,
        clip: Clipboard,
        signals: mpsc::UnboundedReceiver<ClipSignal>,
        log: Arc<Mutex<ServerLog>>,
        /// Textos que se habrían avisado a la interfaz.
        emitted: Vec<String>,
    }

    impl Wire {
        fn new() -> Self {
            let (tx, signals) = mpsc::unbounded_channel();
            let log = Arc::new(Mutex::new(ServerLog::default()));
            Self {
                client: CliprdrClient::new(Box::new(TextClipboard { tx })),
                server: CliprdrServer::new(Box::new(ServerSide(log.clone()))),
                clip: Clipboard::new(true),
                signals,
                log,
                emitted: vec![],
            }
        }

        fn deliver_to_server(&mut self, messages: Vec<SvcMessage>) {
            for message in messages {
                let payload = message.encode_unframed_pdu().unwrap();
                let replies = self.server.process(&payload).unwrap();
                self.deliver_to_client(replies);
            }
        }

        fn deliver_to_client(&mut self, messages: Vec<SvcMessage>) {
            for message in messages {
                let payload = message.encode_unframed_pdu().unwrap();
                let replies = self.client.process(&payload).unwrap();
                self.deliver_to_server(replies);
            }
            // Lo que haría el bucle de la sesión con los avisos del canal.
            while let Ok(signal) = self.signals.try_recv() {
                let reply = self.clip.on_signal(signal, &mut self.client);
                self.emitted.extend(reply.text);
                if let Some(messages) = reply.messages {
                    self.deliver_to_server(messages.into());
                }
            }
        }

        /// El remoto pega: pide el texto del portapapeles del cliente.
        fn remote_paste(&mut self, format: ClipboardFormatId) -> Option<String> {
            let request = self.server.initiate_paste(format).unwrap();
            self.deliver_to_client(request.into());
            self.log.lock().unwrap().data.pop().expect("sin respuesta")
        }

        /// El remoto copia algo en estos formatos.
        fn remote_copy(&mut self, formats: &[ClipboardFormatId]) {
            let formats: Vec<_> = formats.iter().copied().map(ClipboardFormat::new).collect();
            let list = self.server.initiate_copy(&formats).unwrap();
            self.deliver_to_client(list.into());
        }

        /// El remoto entrega el texto que el cliente le ha pedido.
        fn remote_delivers(&mut self, text: &str) {
            let data = self
                .server
                .submit_format_data(OwnedFormatDataResponse::new_unicode_string(text))
                .unwrap();
            self.deliver_to_client(data.into());
        }

        fn local_copy(&mut self, text: &str) -> bool {
            match self.clip.local_copy(text.into(), &mut self.client) {
                Some(messages) => {
                    self.deliver_to_server(messages.into());
                    true
                }
                None => false,
            }
        }
    }

    #[test]
    fn clipboard_text_travels_both_ways() {
        const TEXT: ClipboardFormatId = ClipboardFormatId::CF_UNICODETEXT;
        const IMAGE: ClipboardFormatId = ClipboardFormatId::CF_DIB;
        let mut wire = Wire::new();

        // Texto local conocido antes de conectar: se anuncia al inicializar
        // el canal (antes, una copia local no envía nada).
        assert!(!wire.local_copy("previo"));
        let hello = wire.server.start().unwrap();
        wire.deliver_to_client(hello);
        assert!(wire.client.is_ready());
        assert_eq!(wire.log.lock().unwrap().copies, [[TEXT]]);
        assert_eq!(wire.remote_paste(TEXT).as_deref(), Some("previo"));

        // Copia local: se anuncia y se sirve con los saltos de Windows.
        assert!(wire.local_copy("uno\ndos ñ€😀"));
        assert_eq!(wire.log.lock().unwrap().copies, [[TEXT], [TEXT]]);
        assert_eq!(wire.remote_paste(TEXT).as_deref(), Some("uno\r\ndos ñ€😀"));
        // Un formato que no se ofrece se contesta con un error.
        assert_eq!(wire.remote_paste(IMAGE), None);

        // El remoto copia texto: se trae enseguida y se avisa a la interfaz.
        wire.remote_copy(&[IMAGE, TEXT]);
        assert_eq!(wire.log.lock().unwrap().requests, [TEXT]);
        wire.remote_delivers("remoto ñ\r\nfin");
        let expected = if cfg!(windows) {
            "remoto ñ\r\nfin"
        } else {
            "remoto ñ\nfin"
        };
        assert_eq!(wire.emitted, [expected]);
        // La interfaz lo pone en su portapapeles y nos lo devuelve como
        // «texto local»: es un eco y no se vuelve a anunciar.
        assert!(!wire.local_copy(expected));
        assert_eq!(wire.log.lock().unwrap().copies.len(), 2);
        // El remoto vuelve a copiar lo mismo: no hay nada nuevo que avisar.
        wire.remote_copy(&[TEXT]);
        wire.remote_delivers("remoto ñ\r\nfin");
        assert_eq!(wire.emitted.len(), 1);

        // El remoto copia algo que no es texto: no se pide nada.
        wire.remote_copy(&[IMAGE]);
        assert_eq!(wire.log.lock().unwrap().requests, [TEXT, TEXT]);

        // Una copia local posterior (aunque repita un texto remoto antiguo
        // tras otra distinta) sí se anuncia.
        assert!(wire.local_copy("otra"));
        assert!(wire.local_copy(expected));
        assert_eq!(wire.log.lock().unwrap().copies.len(), 4);
        assert_eq!(wire.remote_paste(TEXT).as_deref(), Some("remoto ñ\r\nfin"));
        // Y si el remoto ofrece de vuelta ese mismo texto, tampoco es nuevo.
        wire.remote_copy(&[TEXT]);
        wire.remote_delivers("remoto ñ\r\nfin");
        assert_eq!(wire.emitted.len(), 1);

        // Con el portapapeles desactivado no se anuncia nada.
        wire.clip.enabled = false;
        assert!(!wire.local_copy("secreto"));
    }

    #[test]
    fn newline_conversion() {
        assert_eq!(to_remote_newlines("a\nb\r\nc"), "a\r\nb\r\nc");
        if !cfg!(windows) {
            assert_eq!(to_local_newlines("a\r\nb".into()), "a\nb");
        }
    }
}
