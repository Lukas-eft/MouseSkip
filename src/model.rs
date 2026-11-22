use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

pub const MAX_IN_MEMORY_EVENTS: usize = 10_000;
pub const MAX_UI_EVENTS: usize = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipKind {
    #[serde(rename = "dropout")]
    PollingDropout,
    #[serde(rename = "spinout")]
    SensorSpinout,
}

impl SkipKind {
    pub fn parse_filter(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dropout" => Some(Self::PollingDropout),
            "spinout" => Some(Self::SensorSpinout),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkipEvent {
    pub id: u64,
    pub timestamp_ms: i64,
    pub kind: SkipKind,
    pub dt_ms: f64,
    pub dx: i32,
    pub dy: i32,
    pub delta_px: f64,
    pub estimated_hz: u32,
    pub threshold_used: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThresholdConfig {
    pub dropout_threshold_ms: f64,
    pub spinout_threshold_px: f64,
    pub active_pause_cutoff_ms: f64,
    pub monitoring_enabled: bool,
    pub vibrancy_mode: String,
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            dropout_threshold_ms: 6.0,
            spinout_threshold_px: 1500.0,
            active_pause_cutoff_ms: 60.0,
            monitoring_enabled: true,
            vibrancy_mode: "mica".to_string(),
        }
    }
}

impl ThresholdConfig {
    pub fn clamp_in_place(&mut self) {
        if !self.dropout_threshold_ms.is_finite() {
            self.dropout_threshold_ms = 6.0;
        }
        if !self.spinout_threshold_px.is_finite() {
            self.spinout_threshold_px = 1500.0;
        }
        if !self.active_pause_cutoff_ms.is_finite() {
            self.active_pause_cutoff_ms = 60.0;
        }

        self.dropout_threshold_ms = self.dropout_threshold_ms.clamp(1.5, 100.0);
        self.spinout_threshold_px = self.spinout_threshold_px.clamp(300.0, 5000.0);
        self.active_pause_cutoff_ms = self.active_pause_cutoff_ms.clamp(20.0, 300.0);

        if self.active_pause_cutoff_ms < self.dropout_threshold_ms + 10.0 {
            self.active_pause_cutoff_ms = (self.dropout_threshold_ms + 10.0).min(300.0);
        }
        if self.dropout_threshold_ms > self.active_pause_cutoff_ms - 10.0 {
            self.dropout_threshold_ms = (self.active_pause_cutoff_ms - 10.0).max(1.5);
        }

        self.vibrancy_mode = "mica".to_string();
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigPatch {
    pub dropout_threshold_ms: Option<f64>,
    pub spinout_threshold_px: Option<f64>,
    pub active_pause_cutoff_ms: Option<f64>,
    pub monitoring_enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    Last15m,
    Last1h,
    Last24h,
    All,
}

impl TimeRange {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "15m" => Self::Last15m,
            "1h" => Self::Last1h,
            "24h" => Self::Last24h,
            "all" => Self::All,
            _ => Self::Last15m,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Last15m => "15m",
            Self::Last1h => "1h",
            Self::Last24h => "24h",
            Self::All => "all",
        }
    }

    pub fn duration_ms(self) -> Option<i64> {
        match self {
            Self::Last15m => Some(15 * 60 * 1000),
            Self::Last1h => Some(60 * 60 * 1000),
            Self::Last24h => Some(24 * 60 * 60 * 1000),
            Self::All => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSensorMetrics {
    pub total_packets: u64,
    pub last_dx: i32,
    pub last_dy: i32,
    pub last_dt_ms: f64,
    pub avg_dt_ms: f64,
    pub estimated_hz: u32,
    pub raw_input_active: bool,
    pub last_packet_age_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineBucket {
    pub label: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub dropouts: u32,
    pub spinouts: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardResponse {
    pub range: String,
    pub config: ThresholdConfig,
    pub live: LiveSensorMetrics,
    pub total_skips: usize,
    pub dropout_count: usize,
    pub spinout_count: usize,
    pub max_dropout_ms: f64,
    pub max_spinout_px: f64,
    pub skips_per_hour: f64,
    pub total_stored_events: usize,
    pub timeline: Vec<TimelineBucket>,
    pub events: Vec<SkipEvent>,
}

#[derive(Debug)]
struct DeviceState {
    last_move_instant: Option<Instant>,
    active_streak: u32,
    recent_intervals_ms: VecDeque<f64>,
    last_dt_ms: f64,
    avg_dt_ms: f64,
    estimated_hz: u32,
    last_dx: i32,
    last_dy: i32,
    last_delta_px: f64,
    ema_velocity_cpms: f64,
    ema_dx: f64,
    ema_dy: f64,
    peak_streak_delta_px: f64,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            last_move_instant: None,
            active_streak: 0,
            recent_intervals_ms: VecDeque::with_capacity(64),
            last_dt_ms: 0.0,
            avg_dt_ms: 0.0,
            estimated_hz: 0,
            last_dx: 0,
            last_dy: 0,
            last_delta_px: 0.0,
            ema_velocity_cpms: 0.0,
            ema_dx: 0.0,
            ema_dy: 0.0,
            peak_streak_delta_px: 0.0,
        }
    }
}

fn direction_cosine_f64(dx1: f64, dy1: f64, dx2: f64, dy2: f64) -> f64 {
    let n1 = dx1.hypot(dy1);
    let n2 = dx2.hypot(dy2);
    if n1 <= f64::EPSILON || n2 <= f64::EPSILON {
        return 1.0;
    }
    (dx1 * dx2 + dy1 * dy2) / (n1 * n2)
}

fn direction_cosine(dx1: i32, dy1: i32, dx2: i32, dy2: i32) -> f64 {
    direction_cosine_f64(dx1 as f64, dy1 as f64, dx2 as f64, dy2 as f64)
}

#[derive(Debug)]
pub struct SensorAnalyzer {
    devices: HashMap<usize, DeviceState>,
    pub total_packets: u64,
    pub last_dx: i32,
    pub last_dy: i32,
    pub last_dt_ms: f64,
    pub avg_dt_ms: f64,
    pub estimated_hz: u32,
    pub next_event_id: u64,
}

impl Default for SensorAnalyzer {
    fn default() -> Self {
        Self::new(1)
    }
}

impl SensorAnalyzer {
    pub fn new(next_event_id: u64) -> Self {
        Self {
            devices: HashMap::with_capacity(4),
            total_packets: 0,
            last_dx: 0,
            last_dy: 0,
            last_dt_ms: 0.0,
            avg_dt_ms: 0.0,
            estimated_hz: 0,
            next_event_id: next_event_id.max(1),
        }
    }

    #[cfg(test)]
    pub fn process_raw_delta(
        &mut self,
        dx: i32,
        dy: i32,
        now_instant: Instant,
        now_epoch_ms: i64,
        config: &ThresholdConfig,
    ) -> Vec<SkipEvent> {
        self.process_raw_delta_for_device(0, dx, dy, now_instant, now_epoch_ms, config)
    }

    pub fn process_raw_delta_for_device(
        &mut self,
        device_id: usize,
        dx: i32,
        dy: i32,
        now_instant: Instant,
        now_epoch_ms: i64,
        config: &ThresholdConfig,
    ) -> Vec<SkipEvent> {
        if dx == 0 && dy == 0 {
            return Vec::new();
        }

        self.total_packets = self.total_packets.saturating_add(1);
        self.last_dx = dx;
        self.last_dy = dy;
        let delta_px = (dx as f64).hypot(dy as f64);

        if !self.devices.contains_key(&device_id) && self.devices.len() >= 32 {
            if let Some(&evict_key) = self
                .devices
                .iter()
                .min_by_key(|(_, d)| d.last_move_instant)
                .map(|(k, _)| k)
            {
                self.devices.remove(&evict_key);
            }
        }

        let dev = self.devices.entry(device_id).or_default();
        let mut emitted = Vec::new();
        let mut measured_dt_ms = 0.0;

        if let Some(prev) = dev.last_move_instant {
            let dt_ms = now_instant.saturating_duration_since(prev).as_secs_f64() * 1000.0;
            measured_dt_ms = dt_ms;
            dev.last_dt_ms = dt_ms;

            let nominal_dt_ms = if dev.avg_dt_ms > 0.25 {
                dev.avg_dt_ms
            } else {
                1.0
            };
            let fast_cadence_max_dt = (nominal_dt_ms * 2.5)
                .max(2.5)
                .min(config.dropout_threshold_ms);

            let dir_cos = direction_cosine(dev.last_dx, dev.last_dy, dx, dy);
            let streak_dir_cos =
                direction_cosine_f64(dev.ema_dx, dev.ema_dy, dx as f64, dy as f64);
            let gap_velocity_cpms = if dt_ms > 0.0 { delta_px / dt_ms } else { 0.0 };
            let is_os_coalesced = dt_ms > fast_cadence_max_dt
                && dt_ms <= 25.0
                && ((delta_px >= 7.0 && gap_velocity_cpms >= 0.40)
                    || (delta_px >= 4.0
                        && dev.ema_velocity_cpms > 0.0
                        && gap_velocity_cpms >= dev.ema_velocity_cpms * 0.25));
            let is_turnaround_braking = (dt_ms > fast_cadence_max_dt
                && dt_ms <= 17.5
                && delta_px < 7.0)
                || (dev.peak_streak_delta_px >= 5.0
                    && delta_px < dev.peak_streak_delta_px * 0.85)
                || (dev.ema_velocity_cpms >= 4.5
                    && delta_px < dev.ema_velocity_cpms * nominal_dt_ms * 0.85);

            if dt_ms > 0.08
                && dt_ms <= fast_cadence_max_dt.max(9.0)
                && delta_px >= 3.0
                && !is_os_coalesced
                && !is_turnaround_braking
                && (dev.estimated_hz < 400 || dt_ms <= 3.5)
            {
                if dev.recent_intervals_ms.len() >= 64 {
                    dev.recent_intervals_ms.pop_front();
                }
                dev.recent_intervals_ms.push_back(dt_ms);

                let mut sorted: Vec<f64> = dev.recent_intervals_ms.iter().copied().collect();
                sorted.sort_by(|a, b| a.total_cmp(b));
                let take_n = (sorted.len() * 25).div_ceil(100).max(1);
                let sum: f64 = sorted.iter().take(take_n).sum();
                let avg = sum / take_n as f64;
                dev.avg_dt_ms = avg;
                if avg > 0.0 {
                    dev.estimated_hz = (1000.0 / avg).round().clamp(1.0, 16000.0) as u32;
                }
            }

            if config.monitoring_enabled
                && dev.active_streak >= 4
                && delta_px >= 4.0
                && dir_cos >= 0.5
                && streak_dir_cos >= 0.5
                && !is_os_coalesced
                && !is_turnaround_braking
                && dt_ms > config.dropout_threshold_ms
                && dt_ms <= config.active_pause_cutoff_ms
            {
                let event_id = self.next_event_id;
                self.next_event_id = self.next_event_id.saturating_add(1);
                emitted.push(SkipEvent {
                    id: event_id,
                    timestamp_ms: now_epoch_ms,
                    kind: SkipKind::PollingDropout,
                    dt_ms,
                    dx,
                    dy,
                    delta_px,
                    estimated_hz: dev.estimated_hz,
                    threshold_used: config.dropout_threshold_ms,
                });
            }

            if dt_ms > fast_cadence_max_dt || delta_px < 3.0 {
                if is_os_coalesced
                    && !is_turnaround_braking
                    && dir_cos >= 0.5
                    && streak_dir_cos >= 0.5
                {
                    dev.active_streak = dev.active_streak.saturating_add(1);
                    dev.ema_dx = if dev.active_streak > 1 {
                        dev.ema_dx * 0.7 + (dx as f64) * 0.3
                    } else {
                        dx as f64
                    };
                    dev.ema_dy = if dev.active_streak > 1 {
                        dev.ema_dy * 0.7 + (dy as f64) * 0.3
                    } else {
                        dy as f64
                    };
                } else {
                    dev.active_streak = 0;
                    dev.ema_velocity_cpms = 0.0;
                    dev.ema_dx = 0.0;
                    dev.ema_dy = 0.0;
                    dev.peak_streak_delta_px = 0.0;
                }
            } else if dir_cos < 0.35 || streak_dir_cos < 0.35 {
                dev.active_streak = 1;
                dev.ema_velocity_cpms = (delta_px / dt_ms.max(0.25)).min(500.0);
                dev.ema_dx = dx as f64;
                dev.ema_dy = dy as f64;
                dev.peak_streak_delta_px = delta_px;
            } else if is_turnaround_braking
                || (dev.last_delta_px > 0.0 && delta_px < dev.last_delta_px * 0.75)
            {
                dev.active_streak = 0;
                dev.peak_streak_delta_px = dev.peak_streak_delta_px.max(delta_px);
            } else if delta_px >= 4.0 {
                dev.active_streak = dev.active_streak.saturating_add(1);
                let inst_vel = (delta_px / dt_ms.max(0.25)).min(500.0);
                dev.ema_velocity_cpms = if dev.ema_velocity_cpms > 0.0 {
                    dev.ema_velocity_cpms * 0.7 + inst_vel * 0.3
                } else {
                    inst_vel
                };
                dev.ema_dx = if dev.active_streak > 1 {
                    dev.ema_dx * 0.7 + (dx as f64) * 0.3
                } else {
                    dx as f64
                };
                dev.ema_dy = if dev.active_streak > 1 {
                    dev.ema_dy * 0.7 + (dy as f64) * 0.3
                } else {
                    dy as f64
                };
                dev.peak_streak_delta_px = dev.peak_streak_delta_px.max(delta_px);
            }
        } else {
            dev.active_streak = 0;
        }

        if config.monitoring_enabled
            && measured_dt_ms > 0.0
            && measured_dt_ms <= config.active_pause_cutoff_ms
        {
            let nominal_dt_ms = if dev.avg_dt_ms > 0.25 {
                dev.avg_dt_ms
            } else {
                1.0
            };
            let effective_dt_ms = measured_dt_ms.max(nominal_dt_ms).max(0.5);
            let velocity_cpms = delta_px / effective_dt_ms;

            if velocity_cpms >= config.spinout_threshold_px && delta_px >= 300.0 {
                let event_id = self.next_event_id;
                self.next_event_id = self.next_event_id.saturating_add(1);
                emitted.push(SkipEvent {
                    id: event_id,
                    timestamp_ms: now_epoch_ms,
                    kind: SkipKind::SensorSpinout,
                    dt_ms: measured_dt_ms,
                    dx,
                    dy,
                    delta_px: velocity_cpms,
                    estimated_hz: dev.estimated_hz,
                    threshold_used: config.spinout_threshold_px,
                });
            }
        }

        dev.last_move_instant = Some(now_instant);
        dev.last_dx = dx;
        dev.last_dy = dy;
        dev.last_delta_px = delta_px;
        self.last_dt_ms = dev.last_dt_ms;
        if dev.estimated_hz > 0 {
            self.avg_dt_ms = dev.avg_dt_ms;
            self.estimated_hz = dev.estimated_hz;
        }

        emitted
    }
}

pub struct LiveTelemetryAtomics {
    total_packets: AtomicU64,
    last_dx: AtomicI32,
    last_dy: AtomicI32,
    last_dt_bits: AtomicU64,
    avg_dt_bits: AtomicU64,
    estimated_hz: AtomicU32,
    raw_input_active: AtomicBool,
    last_packet_epoch_ms: AtomicI64,
}

impl Default for LiveTelemetryAtomics {
    fn default() -> Self {
        Self {
            total_packets: AtomicU64::new(0),
            last_dx: AtomicI32::new(0),
            last_dy: AtomicI32::new(0),
            last_dt_bits: AtomicU64::new(0.0_f64.to_bits()),
            avg_dt_bits: AtomicU64::new(0.0_f64.to_bits()),
            estimated_hz: AtomicU32::new(0),
            raw_input_active: AtomicBool::new(false),
            last_packet_epoch_ms: AtomicI64::new(0),
        }
    }
}

impl LiveTelemetryAtomics {
    pub fn update_from_analyzer(&self, analyzer: &SensorAnalyzer, now_epoch_ms: i64) {
        self.total_packets
            .store(analyzer.total_packets, Ordering::Relaxed);
        self.last_dx.store(analyzer.last_dx, Ordering::Relaxed);
        self.last_dy.store(analyzer.last_dy, Ordering::Relaxed);
        self.last_dt_bits
            .store(analyzer.last_dt_ms.to_bits(), Ordering::Relaxed);
        self.avg_dt_bits
            .store(analyzer.avg_dt_ms.to_bits(), Ordering::Relaxed);
        self.estimated_hz
            .store(analyzer.estimated_hz, Ordering::Relaxed);
        self.last_packet_epoch_ms
            .store(now_epoch_ms, Ordering::Relaxed);
    }

    pub fn set_raw_input_active(&self, active: bool) {
        self.raw_input_active.store(active, Ordering::Release);
    }

    pub fn snapshot(&self, now_epoch_ms: i64) -> LiveSensorMetrics {
        let last_epoch = self.last_packet_epoch_ms.load(Ordering::Relaxed);
        let age = if last_epoch > 0 {
            Some(now_epoch_ms.saturating_sub(last_epoch) as u64)
        } else {
            None
        };

        LiveSensorMetrics {
            total_packets: self.total_packets.load(Ordering::Relaxed),
            last_dx: self.last_dx.load(Ordering::Relaxed),
            last_dy: self.last_dy.load(Ordering::Relaxed),
            last_dt_ms: f64::from_bits(self.last_dt_bits.load(Ordering::Relaxed)),
            avg_dt_ms: f64::from_bits(self.avg_dt_bits.load(Ordering::Relaxed)),
            estimated_hz: self.estimated_hz.load(Ordering::Relaxed),
            raw_input_active: self.raw_input_active.load(Ordering::Acquire),
            last_packet_age_ms: age,
        }
    }
}

pub struct SharedAppState {
    pub config: RwLock<ThresholdConfig>,
    pub events: Mutex<Vec<SkipEvent>>,
    pub analyzer: Mutex<SensorAnalyzer>,
    pub live: LiveTelemetryAtomics,
    pub store_io_lock: Mutex<()>,
    pub dirty: Arc<AtomicBool>,
    pub raw_input_registered: AtomicBool,
}

impl SharedAppState {
    pub fn new(config: ThresholdConfig, mut events: Vec<SkipEvent>) -> Arc<Self> {
        let mut cfg = config;
        cfg.clamp_in_place();

        if events.len() > MAX_IN_MEMORY_EVENTS {
            let overflow = events.len() - MAX_IN_MEMORY_EVENTS;
            events.drain(0..overflow);
        }

        let next_id = events.iter().map(|e| e.id).max().unwrap_or(0) + 1;

        Arc::new(Self {
            config: RwLock::new(cfg),
            events: Mutex::new(events),
            analyzer: Mutex::new(SensorAnalyzer::new(next_id)),
            live: LiveTelemetryAtomics::default(),
            store_io_lock: Mutex::new(()),
            dirty: Arc::new(AtomicBool::new(false)),
            raw_input_registered: AtomicBool::new(false),
        })
    }

    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    pub fn record_raw_packet(
        &self,
        device_id: usize,
        dx: i32,
        dy: i32,
        now_instant: Instant,
        now_epoch_ms: i64,
    ) {
        let cfg = match self.config.read() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };

        let emitted = if let Ok(mut analyzer) = self.analyzer.lock() {
            let evs = analyzer.process_raw_delta_for_device(
                device_id,
                dx,
                dy,
                now_instant,
                now_epoch_ms,
                &cfg,
            );
            self.live.update_from_analyzer(&analyzer, now_epoch_ms);
            evs
        } else {
            return;
        };

        if !emitted.is_empty() {
            if let Ok(mut events) = self.events.lock() {
                events.extend(emitted);
                if events.len() > MAX_IN_MEMORY_EVENTS {
                    let overflow = events.len() - MAX_IN_MEMORY_EVENTS;
                    events.drain(0..overflow);
                }
            }
            self.mark_dirty();
        }
    }

    pub fn snapshot_dashboard(
        &self,
        range: TimeRange,
        kind_filter: Option<SkipKind>,
        now_epoch_ms: i64,
    ) -> Result<DashboardResponse, &'static str> {
        let cfg = self
            .config
            .read()
            .map_err(|_| "config_locked")?
            .clone();
        let live = self.live.snapshot(now_epoch_ms);
        let events_guard = self.events.lock().map_err(|_| "events_locked")?;

        Ok(build_dashboard_response(
            &cfg,
            &events_guard,
            live,
            range,
            kind_filter,
            now_epoch_ms,
        ))
    }
}

fn format_bucket_label(ts_ms: i64, include_date: bool) -> String {
    let dt_utc = DateTime::from_timestamp_millis(ts_ms)
        .unwrap_or_else(|| DateTime::from_timestamp_millis(0).expect("epoch"));
    let dt_local: DateTime<Local> = Local.from_utc_datetime(&dt_utc.naive_utc());
    if include_date {
        dt_local.format("%d.%m %H:%M").to_string()
    } else {
        dt_local.format("%H:%M").to_string()
    }
}

pub fn build_dashboard_response(
    config: &ThresholdConfig,
    all_events: &[SkipEvent],
    live: LiveSensorMetrics,
    range: TimeRange,
    kind_filter: Option<SkipKind>,
    now_epoch_ms: i64,
) -> DashboardResponse {
    let cutoff_ms = range.duration_ms().map(|d| now_epoch_ms - d);

    let filtered: Vec<&SkipEvent> = all_events
        .iter()
        .filter(|e| match cutoff_ms {
            Some(c) => e.timestamp_ms >= c && e.timestamp_ms <= now_epoch_ms + 5_000,
            None => e.timestamp_ms <= now_epoch_ms + 5_000,
        })
        .collect();

    let mut dropout_count = 0usize;
    let mut spinout_count = 0usize;
    let mut max_dropout_ms = 0.0_f64;
    let mut max_spinout_px = 0.0_f64;

    for ev in &filtered {
        match ev.kind {
            SkipKind::PollingDropout => {
                dropout_count += 1;
                if ev.dt_ms > max_dropout_ms {
                    max_dropout_ms = ev.dt_ms;
                }
            }
            SkipKind::SensorSpinout => {
                spinout_count += 1;
                if ev.delta_px > max_spinout_px {
                    max_spinout_px = ev.delta_px;
                }
            }
        }
    }

    let total_skips = filtered.len();

    let span_hours = match range {
        TimeRange::Last15m => 0.25,
        TimeRange::Last1h => 1.0,
        TimeRange::Last24h => 24.0,
        TimeRange::All => {
            if let Some(min_ts) = filtered.iter().map(|e| e.timestamp_ms).min() {
                ((now_epoch_ms - min_ts).max(60_000) as f64) / 3_600_000.0
            } else {
                1.0
            }
        }
    };
    let skips_per_hour = if span_hours > 0.0 {
        total_skips as f64 / span_hours
    } else {
        0.0
    };

    let timeline = build_timeline_buckets(&filtered, range, now_epoch_ms);

    let events: Vec<SkipEvent> = filtered
        .iter()
        .rev()
        .filter(|e| match kind_filter {
            Some(k) => e.kind == k,
            None => true,
        })
        .take(MAX_UI_EVENTS)
        .map(|e| (*e).clone())
        .collect();

    DashboardResponse {
        range: range.as_str().to_string(),
        config: config.clone(),
        live,
        total_skips,
        dropout_count,
        spinout_count,
        max_dropout_ms,
        max_spinout_px,
        skips_per_hour,
        total_stored_events: all_events.len(),
        timeline,
        events,
    }
}

pub fn build_timeline_buckets(
    events: &[&SkipEvent],
    range: TimeRange,
    now_epoch_ms: i64,
) -> Vec<TimelineBucket> {
    let (bucket_count, start_ms, bucket_width_ms, include_date) = match range {
        TimeRange::Last15m => {
            let count = 15usize;
            let width = 60_000i64;
            (count, now_epoch_ms - (count as i64) * width, width, false)
        }
        TimeRange::Last1h => {
            let count = 12usize;
            let width = 5 * 60_000i64;
            (count, now_epoch_ms - (count as i64) * width, width, false)
        }
        TimeRange::Last24h => {
            let count = 24usize;
            let width = 60 * 60_000i64;
            (count, now_epoch_ms - (count as i64) * width, width, false)
        }
        TimeRange::All => {
            let min_ts = events.iter().map(|e| e.timestamp_ms).min();
            let span = min_ts.map(|m| now_epoch_ms - m).unwrap_or(0);
            if span <= 15 * 60_000 {
                let count = 15usize;
                let width = 60_000i64;
                (count, now_epoch_ms - (count as i64) * width, width, false)
            } else {
                let count = 16usize;
                let width = ((span as u64).div_ceil(count as u64) as i64).max(1_000);
                let first_ts = now_epoch_ms - (count as i64) * width;
                let show_date = span > 24 * 60 * 60_000;
                (count, first_ts, width, show_date)
            }
        }
    };

    let mut buckets: Vec<TimelineBucket> = (0..bucket_count)
        .map(|i| {
            let b_start = start_ms + (i as i64) * bucket_width_ms;
            let b_end = b_start + bucket_width_ms;
            TimelineBucket {
                label: format_bucket_label(b_start, include_date),
                start_ms: b_start,
                end_ms: b_end,
                dropouts: 0,
                spinouts: 0,
                total: 0,
            }
        })
        .collect();

    for ev in events {
        if ev.timestamp_ms < start_ms {
            continue;
        }
        let raw_idx = ((ev.timestamp_ms - start_ms) / bucket_width_ms) as usize;
        let idx = raw_idx.min(bucket_count.saturating_sub(1));
        if let Some(bucket) = buckets.get_mut(idx) {
            match ev.kind {
                SkipKind::PollingDropout => bucket.dropouts += 1,
                SkipKind::SensorSpinout => bucket.spinouts += 1,
            }
            bucket.total += 1;
        }
    }

    buckets
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn dummy_live() -> LiveSensorMetrics {
        LiveSensorMetrics {
            total_packets: 0,
            last_dx: 0,
            last_dy: 0,
            last_dt_ms: 0.0,
            avg_dt_ms: 0.0,
            estimated_hz: 0,
            raw_input_active: true,
            last_packet_age_ms: None,
        }
    }

    #[test]
    fn regular_1000hz_stream_and_slow_micro_drift_keeps_1000hz_estimate() {
        let mut analyzer = SensorAnalyzer::default();
        let config = ThresholdConfig::default();
        let t0 = Instant::now();

        for i in 0..50u64 {
            let t = t0 + Duration::from_micros(i * 1000);
            let skips = analyzer.process_raw_delta(4, -2, t, 1_700_000_000_000 + i as i64, &config);
            assert!(skips.is_empty(), "expected no skip at packet {i}");
        }

        for j in 1..=10u64 {
            let t = t0 + Duration::from_micros(50_000 + j * 12_000);
            let skips =
                analyzer.process_raw_delta(0, -1, t, 1_700_000_000_050 + j as i64 * 12, &config);
            assert!(skips.is_empty());
        }

        assert_eq!(analyzer.total_packets, 60);
        assert!(
            (950..=1050).contains(&analyzer.estimated_hz),
            "estimated Hz was {}",
            analyzer.estimated_hz
        );
    }

    #[test]
    fn dropout_requires_four_fast_packets_and_respects_slider_adjustment() {
        let mut analyzer = SensorAnalyzer::default();
        let mut config = ThresholdConfig {
            dropout_threshold_ms: 15.0,
            active_pause_cutoff_ms: 60.0,
            ..Default::default()
        };
        let t0 = Instant::now();

        analyzer.process_raw_delta(5, 0, t0, 1_000_000, &config);
        analyzer.process_raw_delta(5, 0, t0 + Duration::from_millis(1), 1_000_001, &config);
        analyzer.process_raw_delta(5, 0, t0 + Duration::from_millis(2), 1_000_002, &config);
        let early_gap =
            analyzer.process_raw_delta(6, 1, t0 + Duration::from_millis(30), 1_000_030, &config);
        assert!(early_gap.is_empty(), "dropout triggered with streak < 4");

        let t1 = t0 + Duration::from_millis(30);
        for i in 1..=4u64 {
            assert!(analyzer
                .process_raw_delta(
                    5,
                    0,
                    t1 + Duration::from_millis(i),
                    1_000_030 + i as i64,
                    &config
                )
                .is_empty());
        }

        let skips =
            analyzer.process_raw_delta(6, 1, t1 + Duration::from_millis(32), 1_000_062, &config);
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].kind, SkipKind::PollingDropout);
        assert!((skips[0].dt_ms - 28.0).abs() < 0.01);

        let t2 = t1 + Duration::from_millis(32);
        config.dropout_threshold_ms = 35.0;
        for i in 1..=4u64 {
            assert!(analyzer
                .process_raw_delta(
                    5,
                    0,
                    t2 + Duration::from_millis(i),
                    1_000_062 + i as i64,
                    &config
                )
                .is_empty());
        }
        let skips_after_raise =
            analyzer.process_raw_delta(6, 1, t2 + Duration::from_millis(32), 1_000_094, &config);
        assert!(skips_after_raise.is_empty());
    }

    #[test]
    fn micro_movements_do_not_trigger_dropouts_and_reset_streak() {
        let mut analyzer = SensorAnalyzer::default();
        let config = ThresholdConfig::default();
        let t0 = Instant::now();

        analyzer.process_raw_delta(4, 2, t0, 1_000_000, &config);
        for i in 1..=4u64 {
            analyzer.process_raw_delta(
                4,
                2,
                t0 + Duration::from_millis(i),
                1_000_000 + i as i64,
                &config,
            );
        }

        let micro_skip =
            analyzer.process_raw_delta(0, -1, t0 + Duration::from_millis(49), 1_000_049, &config);
        assert!(micro_skip.is_empty());

        let mut analyzer2 = SensorAnalyzer::default();
        analyzer2.process_raw_delta(4, 2, t0, 1_000_000, &config);
        for i in 1..=4u64 {
            analyzer2.process_raw_delta(
                4,
                2,
                t0 + Duration::from_millis(i),
                1_000_000 + i as i64,
                &config,
            );
        }
        analyzer2.process_raw_delta(-1, 0, t0 + Duration::from_millis(5), 1_000_005, &config);
        let after_reset =
            analyzer2.process_raw_delta(5, 0, t0 + Duration::from_millis(25), 1_000_025, &config);
        assert!(after_reset.is_empty());
    }

    #[test]
    fn intentional_pause_over_cutoff_does_not_trigger_dropout_or_spinout() {
        let mut analyzer = SensorAnalyzer::default();
        let config = ThresholdConfig::default();
        let t0 = Instant::now();

        analyzer.process_raw_delta(4, 2, t0, 1_000_000, &config);
        for i in 1..=5u64 {
            analyzer.process_raw_delta(
                4,
                2,
                t0 + Duration::from_millis(i),
                1_000_000 + i as i64,
                &config,
            );
        }

        let skips = analyzer.process_raw_delta(
            2000,
            0,
            t0 + Duration::from_millis(125),
            1_000_125,
            &config,
        );
        assert!(skips.is_empty(), "post-pause packet triggered false skip: {skips:?}");
    }

    #[test]
    fn velocity_normalized_spinout_avoids_125hz_false_alarms_and_catches_anomalies() {
        let mut analyzer = SensorAnalyzer::default();
        let config = ThresholdConfig::default();
        let t0 = Instant::now();

        for i in 0..10u64 {
            let t = t0 + Duration::from_millis(i * 8);
            let skips = analyzer.process_raw_delta(15, 0, t, 1_000_000 + (i * 8) as i64, &config);
            assert!(skips.is_empty());
        }
        let fast_flick_125hz =
            analyzer.process_raw_delta(800, 0, t0 + Duration::from_millis(80), 1_000_080, &config);
        assert!(fast_flick_125hz.is_empty());

        let mut analyzer_1khz = SensorAnalyzer::default();
        for i in 0..10u64 {
            let t = t0 + Duration::from_millis(i);
            analyzer_1khz.process_raw_delta(5, 0, t, 1_000_000 + i as i64, &config);
        }
        let spinout = analyzer_1khz.process_raw_delta(
            1800,
            -900,
            t0 + Duration::from_millis(10),
            1_000_010,
            &config,
        );
        assert_eq!(spinout.len(), 1);
        assert_eq!(spinout[0].kind, SkipKind::SensorSpinout);
        assert!(spinout[0].delta_px > 1500.0);
    }

    #[test]
    fn devices_with_different_handles_are_isolated() {
        let mut analyzer = SensorAnalyzer::default();
        let config = ThresholdConfig::default();
        let t0 = Instant::now();

        for i in 0..=4u64 {
            analyzer.process_raw_delta_for_device(
                101,
                5,
                0,
                t0 + Duration::from_millis(i),
                1_000_000 + i as i64,
                &config,
            );
        }

        let d2 = analyzer.process_raw_delta_for_device(
            202,
            4,
            0,
            t0 + Duration::from_micros(4500),
            1_000_004,
            &config,
        );
        assert!(d2.is_empty());

        let d1 = analyzer.process_raw_delta_for_device(
            101,
            5,
            0,
            t0 + Duration::from_millis(5),
            1_000_005,
            &config,
        );
        assert!(d1.is_empty());
        assert!((analyzer.last_dt_ms - 1.0).abs() < 0.01);
    }

    #[test]
    fn clamp_enforces_slider_bounds_and_cutoff_above_dropout_invariant() {
        let mut cfg = ThresholdConfig {
            dropout_threshold_ms: 95.0,
            active_pause_cutoff_ms: 40.0,
            spinout_threshold_px: 100.0,
            ..Default::default()
        };
        cfg.clamp_in_place();
        assert!((cfg.dropout_threshold_ms - 95.0).abs() < 0.01);
        assert!(cfg.active_pause_cutoff_ms >= cfg.dropout_threshold_ms + 10.0);
        assert_eq!(cfg.spinout_threshold_px, 300.0);

        let mut cfg2 = ThresholdConfig {
            dropout_threshold_ms: 200.0,
            active_pause_cutoff_ms: 20.0,
            spinout_threshold_px: 9999.0,
            ..Default::default()
        };
        cfg2.clamp_in_place();
        assert_eq!(cfg2.dropout_threshold_ms, 100.0);
        assert_eq!(cfg2.active_pause_cutoff_ms, 110.0);
        assert_eq!(cfg2.spinout_threshold_px, 5000.0);
    }

    #[test]
    fn zero_delta_reports_are_ignored() {
        let mut analyzer = SensorAnalyzer::default();
        let config = ThresholdConfig::default();
        let t0 = Instant::now();

        let skips = analyzer.process_raw_delta(0, 0, t0, 1_000_000, &config);
        assert!(skips.is_empty());
        assert_eq!(analyzer.total_packets, 0);
    }

    #[test]
    fn timeline_all_range_never_drops_oldest_event_on_non_divisible_span() {
        let now_ms = 1_760_000_000_000i64;
        let oldest_ts = now_ms - (5 * 60 * 60_000 + 7_123);
        let events = vec![
            SkipEvent {
                id: 1,
                timestamp_ms: oldest_ts,
                kind: SkipKind::PollingDropout,
                dt_ms: 14.0,
                dx: 5,
                dy: 0,
                delta_px: 5.0,
                estimated_hz: 1000,
                threshold_used: 6.0,
            },
            SkipEvent {
                id: 2,
                timestamp_ms: now_ms - 60_000,
                kind: SkipKind::SensorSpinout,
                dt_ms: 1.0,
                dx: 1600,
                dy: 0,
                delta_px: 1600.0,
                estimated_hz: 1000,
                threshold_used: 1500.0,
            },
        ];

        let cfg = ThresholdConfig::default();
        let resp = build_dashboard_response(
            &cfg,
            &events,
            dummy_live(),
            TimeRange::All,
            None,
            now_ms,
        );
        assert_eq!(resp.total_skips, 2);
        assert_eq!(resp.timeline.len(), 16);
        assert_eq!(
            resp.timeline.iter().map(|b| b.total).sum::<u32>(),
            2,
            "oldest event was dropped from timeline"
        );
    }

    #[test]
    fn kind_filter_applies_before_250_event_truncation() {
        let now_ms = 1_760_000_000_000i64;
        let mut events = Vec::new();
        for i in 0..3u64 {
            events.push(SkipEvent {
                id: i + 1,
                timestamp_ms: now_ms - 500_000 + i as i64 * 1000,
                kind: SkipKind::SensorSpinout,
                dt_ms: 1.0,
                dx: 1700,
                dy: 0,
                delta_px: 1700.0,
                estimated_hz: 1000,
                threshold_used: 1500.0,
            });
        }
        for i in 0..260u64 {
            events.push(SkipEvent {
                id: 10 + i,
                timestamp_ms: now_ms - 260_000 + i as i64 * 500,
                kind: SkipKind::PollingDropout,
                dt_ms: 12.0,
                dx: 5,
                dy: 0,
                delta_px: 5.0,
                estimated_hz: 1000,
                threshold_used: 6.0,
            });
        }

        let cfg = ThresholdConfig::default();
        let resp_spinouts = build_dashboard_response(
            &cfg,
            &events,
            dummy_live(),
            TimeRange::Last15m,
            Some(SkipKind::SensorSpinout),
            now_ms,
        );
        assert_eq!(resp_spinouts.spinout_count, 3);
        assert_eq!(resp_spinouts.events.len(), 3);
        assert!(resp_spinouts
            .events
            .iter()
            .all(|e| e.kind == SkipKind::SensorSpinout));
    }

    #[test]
    fn secondary_device_micro_packet_does_not_reset_hz_to_zero() {
        let mut analyzer = SensorAnalyzer::default();
        let config = ThresholdConfig::default();
        let t0 = Instant::now();

        for i in 0..10u64 {
            analyzer.process_raw_delta_for_device(
                101,
                5,
                0,
                t0 + Duration::from_millis(i),
                1_000_000 + i as i64,
                &config,
            );
        }
        assert!((950..=1050).contains(&analyzer.estimated_hz));
        let prev_hz = analyzer.estimated_hz;

        analyzer.process_raw_delta_for_device(
            202,
            0,
            1,
            t0 + Duration::from_millis(12),
            1_000_012,
            &config,
        );
        assert_eq!(analyzer.estimated_hz, prev_hz);
    }

    #[test]
    fn time_range_all_excludes_future_timestamps_and_uses_min_timestamp() {
        let now_ms = 1_760_000_000_000i64;
        let events = vec![
            SkipEvent {
                id: 1,
                timestamp_ms: now_ms - 120_000,
                kind: SkipKind::PollingDropout,
                dt_ms: 10.0,
                dx: 5,
                dy: 0,
                delta_px: 5.0,
                estimated_hz: 1000,
                threshold_used: 6.0,
            },
            SkipEvent {
                id: 2,
                timestamp_ms: now_ms - 3_600_000,
                kind: SkipKind::PollingDropout,
                dt_ms: 11.0,
                dx: 5,
                dy: 0,
                delta_px: 5.0,
                estimated_hz: 1000,
                threshold_used: 6.0,
            },
            SkipEvent {
                id: 3,
                timestamp_ms: now_ms + 600_000,
                kind: SkipKind::PollingDropout,
                dt_ms: 12.0,
                dx: 5,
                dy: 0,
                delta_px: 5.0,
                estimated_hz: 1000,
                threshold_used: 6.0,
            },
        ];

        let cfg = ThresholdConfig::default();
        let resp = build_dashboard_response(
            &cfg,
            &events,
            dummy_live(),
            TimeRange::All,
            None,
            now_ms,
        );
        assert_eq!(resp.total_skips, 2);
        assert!((resp.skips_per_hour - 2.0).abs() < 0.01);
    }

    #[test]
    fn fast_back_and_forth_swipes_with_turnarounds_and_coalescing_do_not_trigger_dropouts() {
        let mut analyzer = SensorAnalyzer::default();
        let config = ThresholdConfig::default();
        let t0 = Instant::now();
        let mut t_us = 0u64;

        for _ in 0..12 {
            let skips = analyzer.process_raw_delta(
                38,
                5,
                t0 + Duration::from_micros(t_us),
                1_760_000_000_000 + (t_us / 1000) as i64,
                &config,
            );
            assert!(skips.is_empty(), "unexpected skip during rightward swipe: {skips:?}");
            t_us += 1_000;
        }

        t_us += 9_800;
        let coalesced_skips = analyzer.process_raw_delta(
            407,
            57,
            t0 + Duration::from_micros(t_us),
            1_760_000_000_000 + (t_us / 1000) as i64,
            &config,
        );
        assert!(
            coalesced_skips.is_empty(),
            "OS-coalesced packet triggered false skip: {coalesced_skips:?}"
        );

        for _ in 0..5 {
            t_us += 1_000;
            let skips = analyzer.process_raw_delta(
                36,
                4,
                t0 + Duration::from_micros(t_us),
                1_760_000_000_000 + (t_us / 1000) as i64,
                &config,
            );
            assert!(skips.is_empty());
        }

        t_us += 15_000;
        let reversal1_skips = analyzer.process_raw_delta(
            -177,
            28,
            t0 + Duration::from_micros(t_us),
            1_760_000_000_000 + (t_us / 1000) as i64,
            &config,
        );
        assert!(
            reversal1_skips.is_empty(),
            "turnaround reversal 1 triggered false skip: {reversal1_skips:?}"
        );

        for _ in 0..10 {
            t_us += 1_000;
            let skips = analyzer.process_raw_delta(
                -35,
                4,
                t0 + Duration::from_micros(t_us),
                1_760_000_000_000 + (t_us / 1000) as i64,
                &config,
            );
            assert!(skips.is_empty());
        }

        t_us += 18_000;
        let reversal2_skips = analyzer.process_raw_delta(
            45,
            -6,
            t0 + Duration::from_micros(t_us),
            1_760_000_000_000 + (t_us / 1000) as i64,
            &config,
        );
        assert!(
            reversal2_skips.is_empty(),
            "turnaround reversal 2 triggered false skip: {reversal2_skips:?}"
        );

        for _ in 0..8 {
            t_us += 1_000;
            let skips = analyzer.process_raw_delta(
                20,
                -4,
                t0 + Duration::from_micros(t_us),
                1_760_000_000_000 + (t_us / 1000) as i64,
                &config,
            );
            assert!(skips.is_empty());
        }

        t_us += 8_045;
        let curved_skips = analyzer.process_raw_delta(
            -1,
            -21,
            t0 + Duration::from_micros(t_us),
            1_760_000_000_000 + (t_us / 1000) as i64,
            &config,
        );
        assert!(
            curved_skips.is_empty(),
            "curved turnaround packet triggered false skip: {curved_skips:?}"
        );

        for _ in 0..6 {
            t_us += 1_000;
            let skips = analyzer.process_raw_delta(
                4,
                -10,
                t0 + Duration::from_micros(t_us),
                1_760_000_000_000 + (t_us / 1000) as i64,
                &config,
            );
            assert!(skips.is_empty());
        }

        t_us += 10_980;
        let burst1_skips = analyzer.process_raw_delta(
            18,
            -72,
            t0 + Duration::from_micros(t_us),
            1_760_000_000_000 + (t_us / 1000) as i64,
            &config,
        );
        assert!(
            burst1_skips.is_empty(),
            "coalesced burst 1 triggered false skip: {burst1_skips:?}"
        );

        for _ in 0..6 {
            t_us += 1_000;
            let skips = analyzer.process_raw_delta(
                -32,
                2,
                t0 + Duration::from_micros(t_us),
                1_760_000_000_000 + (t_us / 1000) as i64,
                &config,
            );
            assert!(skips.is_empty());
        }

        t_us += 14_980;
        let burst2_skips = analyzer.process_raw_delta(
            -472,
            17,
            t0 + Duration::from_micros(t_us),
            1_760_000_000_000 + (t_us / 1000) as i64,
            &config,
        );
        assert!(
            burst2_skips.is_empty(),
            "coalesced burst 2 triggered false skip: {burst2_skips:?}"
        );

        let run2_sequence = [
            (9_070u64, 4i32, 0i32),
            (14_110, -11, 2),
            (10_030, 3, -1),
            (16_040, 5, -1),
            (8_000, 13, 8),
        ];
        for (gap_us, dx, dy) in run2_sequence {
            for _ in 0..6 {
                t_us += 1_000;
                let s = analyzer.process_raw_delta(
                    dx.signum() * 14,
                    0,
                    t0 + Duration::from_micros(t_us),
                    1_760_000_000_000 + (t_us / 1000) as i64,
                    &config,
                );
                assert!(s.is_empty());
            }
            t_us += gap_us;
            let s = analyzer.process_raw_delta(
                dx,
                dy,
                t0 + Duration::from_micros(t_us),
                1_760_000_000_000 + (t_us / 1000) as i64,
                &config,
            );
            assert!(
                s.is_empty(),
                "Run 2 turnaround/braking packet (dt={gap_us}us, dx={dx}, dy={dy}) triggered false skip: {s:?}"
            );
        }

        for _ in 0..6 {
            t_us += 1_000;
            assert!(analyzer
                .process_raw_delta(
                    6,
                    0,
                    t0 + Duration::from_micros(t_us),
                    1_760_000_000_000 + (t_us / 1000) as i64,
                    &config,
                )
                .is_empty());
        }
        t_us += 7_027;
        let boundary_skips = analyzer.process_raw_delta(
            6,
            -1,
            t0 + Duration::from_micros(t_us),
            1_760_000_000_000 + (t_us / 1000) as i64,
            &config,
        );
        assert!(
            boundary_skips.is_empty(),
            "7.027ms boundary packet triggered false skip: {boundary_skips:?}"
        );

        for d in [20i32, 20, 20, 20, 20, 14, 7, 6, 5, 4] {
            t_us += 1_000;
            assert!(analyzer
                .process_raw_delta(
                    d,
                    0,
                    t0 + Duration::from_micros(t_us),
                    1_760_000_000_000 + (t_us / 1000) as i64,
                    &config,
                )
                .is_empty());
        }
        t_us += 35_000;
        let pause_restart_skips = analyzer.process_raw_delta(
            4,
            1,
            t0 + Duration::from_micros(t_us),
            1_760_000_000_000 + (t_us / 1000) as i64,
            &config,
        );
        assert!(
            pause_restart_skips.is_empty(),
            "braking ramp + 35ms hand pause triggered false skip: {pause_restart_skips:?}"
        );

        assert!(
            (950..=1050).contains(&analyzer.estimated_hz),
            "estimated Hz drifted to {}",
            analyzer.estimated_hz
        );
    }
}
