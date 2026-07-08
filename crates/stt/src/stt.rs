//! Speech-to-text: captura o microfone + o áudio do sistema (loopback WASAPI no
//! Windows) em DUAS streams separadas pro Deepgram, para saber QUEM falou cada
//! trecho, e entrega as transcrições como contexto pra IA (ou no prompt).
//!
//! ponytail: chave do Deepgram vem das Settings (KVP) com fallback pra env var.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::{SinkExt, StreamExt, channel::mpsc};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use util::ResultExt as _;

/// Taxa mandada pro Deepgram. 16 kHz mono é o padrão pra STT.
const TARGET_RATE: u32 = 16000;
/// Limite de segmentos guardados (display + contexto). ponytail: evita crescer infinito.
const MAX_SEGMENTS: usize = 60;
/// Quantas barras o waveform do pill mostra.
const WAVEFORM_BARS: usize = 14;

/// Quem falou. `Me` = microfone (você), `Other` = áudio do sistema (interlocutor).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speaker {
    Me,
    Other,
}

impl Speaker {
    pub fn label(&self) -> &'static str {
        match self {
            Speaker::Me => "Você",
            Speaker::Other => "Interlocutor",
        }
    }
    fn idx(self) -> usize {
        match self {
            Speaker::Me => 0,
            Speaker::Other => 1,
        }
    }
}

/// Um trecho finalizado de fala.
#[derive(Clone)]
pub struct Segment {
    pub speaker: Speaker,
    pub text: String,
}

/// Como a transcrição chega na IA.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// Padrão: vai como contexto por baixo dos panos (não entra na caixa de texto).
    Context,
    /// Vai digitada na caixa de mensagem (o usuário revê/edita).
    Prompt,
}

pub enum SttEvent {
    /// Transcrição mudou (parcial ou final) — re-renderiza a barra.
    Updated,
    /// Um trecho finalizado — usado pelo modo Prompt pra inserir no editor.
    FinalSegment(Segment),
    /// Ligou/desligou.
    StateChanged,
    /// Sessão encerrada: transcrição completa formatada (pro registro da reunião).
    /// Vazio se nada foi falado.
    SessionEnded(String),
}

/// Filas de áudio já resampladas pra 16 kHz mono i16, uma por fonte.
#[derive(Default)]
struct SharedAudio {
    mic: Mutex<VecDeque<i16>>,
    system: Mutex<VecDeque<i16>>,
}

struct RawTranscript {
    speaker: Speaker,
    text: String,
    is_final: bool,
}

struct GlobalStt(Entity<Stt>);
impl Global for GlobalStt {}

pub struct Stt {
    running: bool,
    segments: Vec<Segment>,
    /// Registro completo da sessão (não é limpo pelo envio; vira a reunião salva).
    session_log: Vec<Segment>,
    partials: [String; 2],
    delivery: DeliveryMode,
    selected_mic: Option<String>,
    stop_flag: Arc<AtomicBool>,
    /// Quando a sessão começou (pro cronômetro).
    started_at: Option<std::time::Instant>,
    /// Pico de áudio desde a última amostra — separado por fonte pro waveform duplo.
    level_mic: Arc<Mutex<f32>>,
    level_sys: Arc<Mutex<f32>>,
    /// Histórico recente de níveis pra desenhar os dois waveforms sobrepostos.
    waveform_mic: VecDeque<f32>,
    waveform_sys: VecDeque<f32>,
    _tasks: Vec<Task<()>>,
    _capture: Vec<std::thread::JoinHandle<()>>,
}

impl EventEmitter<SttEvent> for Stt {}

pub fn init(cx: &mut App) {
    let entity = cx.new(|_| Stt {
        running: false,
        segments: Vec::new(),
        session_log: Vec::new(),
        partials: [String::new(), String::new()],
        delivery: DeliveryMode::Context,
        selected_mic: None,
        stop_flag: Arc::new(AtomicBool::new(false)),
        started_at: None,
        level_mic: Arc::new(Mutex::new(0.0)),
        level_sys: Arc::new(Mutex::new(0.0)),
        waveform_mic: VecDeque::new(),
        waveform_sys: VecDeque::new(),
        _tasks: Vec::new(),
        _capture: Vec::new(),
    });
    cx.set_global(GlobalStt(entity));
}

impl Stt {
    pub fn global(cx: &App) -> Entity<Stt> {
        cx.global::<GlobalStt>().0.clone()
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn delivery(&self) -> DeliveryMode {
        self.delivery
    }

    pub fn set_delivery(&mut self, mode: DeliveryMode, cx: &mut Context<Self>) {
        self.delivery = mode;
        cx.emit(SttEvent::Updated);
        cx.notify();
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn partial(&self, speaker: Speaker) -> &str {
        &self.partials[speaker.idx()]
    }

    /// Cronômetro formatado MM:SS (vazio se não está rodando).
    pub fn elapsed_label(&self) -> String {
        match self.started_at {
            Some(t) => {
                let secs = t.elapsed().as_secs();
                format!("{:02}:{:02}", secs / 60, secs % 60)
            }
            None => String::new(),
        }
    }

    fn bars_from(dq: &VecDeque<f32>) -> Vec<f32> {
        let mut bars = vec![0.0; WAVEFORM_BARS.saturating_sub(dq.len())];
        bars.extend(dq.iter().map(|v| v.clamp(0.0, 1.0)));
        bars
    }

    /// Barras do waveform do microfone (você).
    pub fn waveform_mic(&self) -> Vec<f32> {
        Self::bars_from(&self.waveform_mic)
    }

    /// Barras do waveform do áudio do sistema (interlocutor).
    pub fn waveform_sys(&self) -> Vec<f32> {
        Self::bars_from(&self.waveform_sys)
    }

    pub fn selected_mic(&self) -> Option<&str> {
        self.selected_mic.as_deref()
    }

    pub fn set_mic(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.selected_mic = name;
        if self.running {
            self.stop(cx);
            self.start(cx);
        }
        cx.notify();
    }

    pub fn has_api_key() -> bool {
        active_key().is_some()
    }

    /// Nomes dos microfones de entrada disponíveis.
    pub fn available_mics() -> Vec<String> {
        cpal::default_host()
            .input_devices()
            .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default()
    }

    /// Transcrição formatada pra IA, agrupando trechos consecutivos do mesmo
    /// falante num parágrafo só, e limpando o buffer. `None` se não há nada.
    pub fn take_transcript_for_ai(&mut self) -> Option<String> {
        if self.segments.is_empty() {
            return None;
        }
        let mut out = String::new();
        let mut last: Option<Speaker> = None;
        for seg in &self.segments {
            if last != Some(seg.speaker) {
                if last.is_some() {
                    out.push('\n');
                }
                out.push_str(seg.speaker.label());
                out.push_str(": ");
                last = Some(seg.speaker);
            } else {
                out.push(' ');
            }
            out.push_str(&seg.text);
        }
        self.segments.clear();
        Some(out)
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.stop(cx);
        } else {
            self.start(cx);
        }
    }

    fn push_segment(&mut self, speaker: Speaker, text: String) {
        self.segments.push(Segment {
            speaker,
            text: text.clone(),
        });
        self.session_log.push(Segment { speaker, text });
        while self.segments.len() > MAX_SEGMENTS {
            self.segments.remove(0);
        }
    }

    /// Formata uma lista de segmentos agrupando trechos do mesmo falante.
    fn format_segments(segments: &[Segment]) -> String {
        let mut out = String::new();
        let mut last: Option<Speaker> = None;
        for seg in segments {
            if last != Some(seg.speaker) {
                if last.is_some() {
                    out.push('\n');
                }
                out.push_str(seg.speaker.label());
                out.push_str(": ");
                last = Some(seg.speaker);
            } else {
                out.push(' ');
            }
            out.push_str(&seg.text);
        }
        out
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self._tasks.clear();
        self._capture.clear();
        self.running = false;
        self.partials = [String::new(), String::new()];
        self.started_at = None;
        self.waveform_mic.clear();
        self.waveform_sys.clear();
        *self.level_mic.lock().unwrap() = 0.0;
        *self.level_sys.lock().unwrap() = 0.0;
        // Emite a transcrição completa da sessão pra virar uma reunião salva.
        let session = Self::format_segments(&self.session_log);
        self.session_log.clear();
        cx.emit(SttEvent::SessionEnded(session));
        cx.emit(SttEvent::StateChanged);
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let provider = selected_provider();
        let Some(api_key) = active_key() else {
            if provider.is_streaming() {
                log::error!("STT: chave do {} não definida", provider.name());
            } else {
                log::error!(
                    "STT: {} ainda não tem streaming implementado — selecione o Deepgram",
                    provider.name()
                );
            }
            return;
        };
        log::info!("STT: iniciando com {} ({} chars)", provider.name(), api_key.len());

        self.session_log.clear();
        self.started_at = Some(std::time::Instant::now());
        self.waveform_mic.clear();
        self.waveform_sys.clear();
        *self.level_mic.lock().unwrap() = 0.0;
        *self.level_sys.lock().unwrap() = 0.0;
        let level_mic = self.level_mic.clone();
        let level_sys = self.level_sys.clone();
        let stop_flag = Arc::new(AtomicBool::new(false));
        self.stop_flag = stop_flag.clone();
        let shared = Arc::new(SharedAudio::default());

        // Threads de captura (cpal Stream é !Send, cada uma na sua thread).
        let mut capture = Vec::new();
        match spawn_capture(
            CaptureKind::Microphone,
            self.selected_mic.clone(),
            shared.clone(),
            level_mic.clone(),
            stop_flag.clone(),
        ) {
            Ok(handle) => capture.push(handle),
            Err(e) => log::error!("STT: falha ao capturar microfone: {e:#}"),
        }
        // ponytail: modo Prompt = ditado (só o meu mic). Só o modo Contexto ("reunião")
        // captura o áudio do sistema (interlocutor).
        let capture_system = self.delivery == DeliveryMode::Context;
        #[cfg(target_os = "windows")]
        if capture_system {
            match spawn_capture(
                CaptureKind::SystemLoopback,
                None,
                shared.clone(),
                level_sys.clone(),
                stop_flag.clone(),
            ) {
                Ok(handle) => capture.push(handle),
                Err(e) => log::warn!("STT: sem loopback do sistema (só mic): {e:#}"),
            }
        }
        self._capture = capture;

        let (msg_tx, mut msg_rx) = mpsc::unbounded::<RawTranscript>();

        // Duas sessões Deepgram: uma pro mic (Você), uma pro sistema (Interlocutor).
        let mic_task = gpui_tokio::Tokio::spawn(cx, {
            let api_key = api_key.clone();
            let shared = shared.clone();
            let stop_flag = stop_flag.clone();
            let tx = msg_tx.clone();
            async move {
                if let Err(e) = run_deepgram(Speaker::Me, api_key, shared, stop_flag, tx).await {
                    log::error!("STT: sessão Deepgram (mic) terminou: {e:#}");
                }
            }
        });
        let sys_task = capture_system.then(|| {
            gpui_tokio::Tokio::spawn(cx, {
                let shared = shared.clone();
                let stop_flag = stop_flag.clone();
                let tx = msg_tx.clone();
                async move {
                    if let Err(e) =
                        run_deepgram(Speaker::Other, api_key, shared, stop_flag, tx).await
                    {
                        log::error!("STT: sessão Deepgram (sistema) terminou: {e:#}");
                    }
                }
            })
        });

        let ui_task = cx.spawn(async move |this, cx| {
            while let Some(raw) = msg_rx.next().await {
                this.update(cx, |stt, cx| {
                    if raw.is_final {
                        let text = raw.text.trim().to_string();
                        if !text.is_empty() {
                            stt.push_segment(raw.speaker, text.clone());
                            cx.emit(SttEvent::FinalSegment(Segment {
                                speaker: raw.speaker,
                                text,
                            }));
                        }
                        stt.partials[raw.speaker.idx()].clear();
                    } else {
                        stt.partials[raw.speaker.idx()] = raw.text;
                    }
                    cx.emit(SttEvent::Updated);
                    cx.notify();
                })
                .ok();
            }
        });

        // Sampler: a cada 100ms amostra o pico → waveform e re-renderiza (cronômetro
        // + barras). Roda enquanto a sessão estiver ativa.
        let sampler = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let keep_going = this
                    .update(cx, |stt, cx| {
                        if !stt.running {
                            return false;
                        }
                        let take = |l: &Arc<Mutex<f32>>| {
                            let mut g = l.lock().unwrap();
                            let v = *g;
                            *g = 0.0;
                            v
                        };
                        stt.waveform_mic.push_back(take(&stt.level_mic));
                        stt.waveform_sys.push_back(take(&stt.level_sys));
                        while stt.waveform_mic.len() > WAVEFORM_BARS {
                            stt.waveform_mic.pop_front();
                        }
                        while stt.waveform_sys.len() > WAVEFORM_BARS {
                            stt.waveform_sys.pop_front();
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        });

        let mut tasks = vec![
            cx.spawn(async move |_, _| {
                mic_task.await.ok();
            }),
            ui_task,
            sampler,
        ];
        if let Some(sys_task) = sys_task {
            tasks.push(cx.spawn(async move |_, _| {
                sys_task.await.ok();
            }));
        }
        self._tasks = tasks;
        self.running = true;
        self.partials = [String::new(), String::new()];
        cx.emit(SttEvent::StateChanged);
        cx.notify();
    }
}

// ================= Provedores de STT (estilo LLM providers) =================

/// Provedores de STT predefinidos. Só o Deepgram tem streaming implementado por ora;
/// os outros guardam a chave e podem ser selecionados (streaming vem depois).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SttProviderKind {
    Deepgram,
    Groq,
    OpenAi,
    ElevenLabs,
}

impl SttProviderKind {
    pub const ALL: [SttProviderKind; 4] = [
        SttProviderKind::Deepgram,
        SttProviderKind::Groq,
        SttProviderKind::OpenAi,
        SttProviderKind::ElevenLabs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SttProviderKind::Deepgram => "Deepgram",
            SttProviderKind::Groq => "Groq (Whisper)",
            SttProviderKind::OpenAi => "OpenAI (Whisper)",
            SttProviderKind::ElevenLabs => "ElevenLabs",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            SttProviderKind::Deepgram => "deepgram",
            SttProviderKind::Groq => "groq",
            SttProviderKind::OpenAi => "openai",
            SttProviderKind::ElevenLabs => "elevenlabs",
        }
    }

    /// Streaming em tempo real implementado?
    pub fn is_streaming(self) -> bool {
        matches!(self, SttProviderKind::Deepgram)
    }

    pub fn from_id(s: &str) -> Option<SttProviderKind> {
        SttProviderKind::ALL.into_iter().find(|k| k.id() == s)
    }

    fn key_kvp(self) -> String {
        format!("stt-key-{}", self.id())
    }
}

/// Chave configurada de um provedor (com fallback pro KVP legado + env do Deepgram).
pub fn provider_key(kind: SttProviderKind) -> Option<String> {
    let kvp = db::kvp::GlobalKeyValueStore::global();
    let stored = kvp
        .read_kvp(&kind.key_kvp())
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty());
    if stored.is_some() {
        return stored;
    }
    if kind == SttProviderKind::Deepgram {
        return kvp
            .read_kvp("deepgram-api-key")
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| {
                std::env::var("DEEPGRAM_API_KEY")
                    .ok()
                    .filter(|k| !k.trim().is_empty())
            });
    }
    None
}

pub fn set_provider_key(kind: SttProviderKind, key: String, cx: &App) {
    let kvp_key = kind.key_kvp();
    cx.background_spawn(async move {
        db::kvp::GlobalKeyValueStore::global()
            .write_kvp(kvp_key, key.trim().to_string())
            .await
            .log_err();
    })
    .detach();
}

/// Provedor ativo (o selecionado, ou Deepgram por padrão).
pub fn selected_provider() -> SttProviderKind {
    db::kvp::GlobalKeyValueStore::global()
        .read_kvp("stt-selected-provider")
        .ok()
        .flatten()
        .and_then(|s| SttProviderKind::from_id(&s))
        .unwrap_or(SttProviderKind::Deepgram)
}

pub fn set_selected_provider(kind: SttProviderKind, cx: &App) {
    let id = kind.id().to_string();
    cx.background_spawn(async move {
        db::kvp::GlobalKeyValueStore::global()
            .write_kvp("stt-selected-provider".to_string(), id)
            .await
            .log_err();
    })
    .detach();
}

/// Chave do provedor ativo, se ele suporta streaming (só Deepgram por ora).
fn active_key() -> Option<String> {
    let sel = selected_provider();
    sel.is_streaming().then(|| provider_key(sel)).flatten()
}

#[derive(Clone, Copy)]
enum CaptureKind {
    Microphone,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    SystemLoopback,
}

fn spawn_capture(
    kind: CaptureKind,
    selected_mic: Option<String>,
    shared: Arc<SharedAudio>,
    level: Arc<Mutex<f32>>,
    stop_flag: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    let host = cpal::default_host();
    let device = match kind {
        CaptureKind::Microphone => selected_mic
            .and_then(|name| {
                host.input_devices()
                    .ok()
                    .and_then(|mut ds| ds.find(|d| d.name().ok().as_deref() == Some(name.as_str())))
            })
            .or_else(|| host.default_input_device())
            .context("nenhum microfone disponível")?,
        // ponytail: loopback WASAPI — no Windows build_input_stream num device de saída
        // captura "o que você ouve". Fora do Windows nem é chamado.
        CaptureKind::SystemLoopback => host
            .default_output_device()
            .context("nenhum dispositivo de saída pra loopback")?,
    };

    // ponytail: config pelo tipo — pro loopback é a config de SAÍDA (o formato que o
    // device toca); usar default_input_config num device de saída devolve formato
    // errado e o loopback vem mudo.
    let supported = match kind {
        CaptureKind::Microphone => device.default_input_config(),
        CaptureKind::SystemLoopback => device.default_output_config(),
    }
    .context("sem config de áudio padrão")?;
    let sample_format = supported.sample_format();
    let in_rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.into();
    log::info!(
        "STT: captura iniciada (fonte={}, rate={in_rate}, canais={channels}, fmt={sample_format:?})",
        match kind {
            CaptureKind::Microphone => "mic",
            CaptureKind::SystemLoopback => "loopback",
        }
    );

    let handle = std::thread::spawn(move || {
        let mut pos: f64 = 0.0;
        let step = in_rate as f64 / TARGET_RATE as f64;

        let shared_cb = shared.clone();
        let level_cb = level.clone();
        let push = move |frames_mono: &[i16]| {
            // Atualiza o pico combinado (mic + sistema) pro waveform.
            let peak = frames_mono
                .iter()
                .map(|s| (s.unsigned_abs() as f32) / 32768.0)
                .fold(0.0f32, f32::max);
            {
                let mut l = level_cb.lock().unwrap();
                *l = l.max(peak);
            }
            let mut queue = match kind {
                CaptureKind::Microphone => shared_cb.mic.lock().unwrap(),
                CaptureKind::SystemLoopback => shared_cb.system.lock().unwrap(),
            };
            queue.extend(frames_mono.iter().copied());
            let max = (TARGET_RATE as usize) * 30;
            while queue.len() > max {
                queue.pop_front();
            }
        };

        let err_fn = |e| log::error!("STT: erro no stream de áudio: {e}");

        let stream_result = match sample_format {
            cpal::SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, channels, &mut pos, step, push, err_fn)
            }
            cpal::SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, channels, &mut pos, step, push, err_fn)
            }
            other => {
                log::error!("STT: formato de amostra não suportado: {other:?}");
                return;
            }
        };

        let stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                log::error!("STT: falha ao abrir stream: {e:#}");
                return;
            }
        };
        if let Err(e) = stream.play() {
            log::error!("STT: falha ao dar play no stream: {e:#}");
            return;
        }

        while !stop_flag.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        drop(stream);
    });

    Ok(handle)
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    pos: &mut f64,
    step: f64,
    mut push: impl FnMut(&[i16]) + Send + 'static,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + ToI16 + Send + 'static,
{
    let mut resample_pos = *pos;
    let mut out: Vec<i16> = Vec::with_capacity(2048);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            out.clear();
            let frames = data.len() / channels.max(1);
            let mut i = resample_pos;
            while (i as usize) < frames {
                let frame = i as usize;
                out.push(data[frame * channels].to_i16());
                i += step;
            }
            resample_pos = i - frames as f64;
            if !out.is_empty() {
                push(&out);
            }
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

trait ToI16 {
    fn to_i16(self) -> i16;
}
impl ToI16 for f32 {
    fn to_i16(self) -> i16 {
        (self.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
    }
}
impl ToI16 for i16 {
    fn to_i16(self) -> i16 {
        self
    }
}

/// Conecta no Deepgram para UMA fonte e devolve transcrições marcadas com o falante.
async fn run_deepgram(
    speaker: Speaker,
    api_key: String,
    shared: Arc<SharedAudio>,
    stop_flag: Arc<AtomicBool>,
    msg_tx: mpsc::UnboundedSender<RawTranscript>,
) -> Result<()> {
    use async_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

    // ponytail: nova-3 + language=pt-BR (modelo PT atualizado do Deepgram) +
    // endpointing=100 (recomendado pra streaming).
    let url = format!(
        "wss://api.deepgram.com/v1/listen?encoding=linear16&sample_rate={TARGET_RATE}\
         &channels=1&model=nova-3&language=pt-BR&interim_results=true&smart_format=true\
         &endpointing=100"
    );
    log::info!("STT: conectando no Deepgram ({})...", speaker.label());
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Token {api_key}"))?,
    );

    let tcp = tokio::net::TcpStream::connect("api.deepgram.com:443")
        .await
        .context("TCP connect Deepgram")?;
    let (ws, _) = async_tungstenite::tokio::client_async_tls_with_connector_and_config(
        request,
        tcp,
        Some(Arc::new(http_client_tls::tls_config()).into()),
        None,
    )
    .await
    .context("handshake WebSocket Deepgram")?;

    log::info!("STT: conectado ao Deepgram ({})", speaker.label());
    let (mut write, mut read) = ws.split();

    let audio_pump = async {
        loop {
            if stop_flag.load(Ordering::SeqCst) {
                let _ = write.send(Message::Close(None)).await;
                break;
            }
            let pcm = drain(&shared, speaker);
            if !pcm.is_empty() {
                let mut bytes = Vec::with_capacity(pcm.len() * 2);
                for s in pcm {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                if write.send(Message::Binary(bytes.into())).await.is_err() {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };

    let reader = async move {
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Some((transcript, is_final)) = parse_deepgram(&text) {
                        if transcript.is_empty() {
                            continue;
                        }
                        if msg_tx
                            .unbounded_send(RawTranscript {
                                speaker,
                                text: transcript,
                                is_final,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                Ok(Message::Close(frame)) => {
                    log::warn!("STT: Deepgram fechou ({}): {frame:?}", speaker.label());
                    break;
                }
                Err(e) => {
                    log::error!("STT: erro lendo do Deepgram ({}): {e}", speaker.label());
                    break;
                }
                _ => {}
            }
        }
    };

    futures::future::join(audio_pump, reader).await;
    Ok(())
}

/// Drena a fila de uma fonte específica.
fn drain(shared: &SharedAudio, speaker: Speaker) -> Vec<i16> {
    let mut queue = match speaker {
        Speaker::Me => shared.mic.lock().unwrap(),
        Speaker::Other => shared.system.lock().unwrap(),
    };
    queue.drain(..).collect()
}

/// Extrai `channel.alternatives[0].transcript` e `is_final` do JSON do Deepgram.
fn parse_deepgram(text: &str) -> Option<(String, bool)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let transcript = value
        .get("channel")?
        .get("alternatives")?
        .get(0)?
        .get("transcript")?
        .as_str()?
        .to_string();
    let is_final = value
        .get("is_final")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some((transcript, is_final))
}
