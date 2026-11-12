//! Statistiques d'usage, en mémoire : appels par outil, erreurs, latence p50/p95, dernier appel, appels réels
//! contre appels du mode test, coupes réseau, validations (approuvées, refusées, expirées). Export JSON local facultatif.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

const KEEP: usize = 1000; // latences gardées par outil

#[derive(Default)]
struct Tool {
    real: u64,
    test: u64,
    errors: u64,
    ms: Vec<u64>,
    last: Option<String>,
}

#[derive(Default)]
struct Inner {
    tools: BTreeMap<String, Tool>,
    egress_blocked: u64,
    mock_writes: u64,
    approved: u64,
    denied: u64,
    timed_out: u64,
    no_confirmation: u64,
    last_call: Option<Value>,
}

pub struct Stats {
    started: Instant,
    inner: Mutex<Inner>,
}

pub enum Validation {
    Approved,
    Denied,
    TimedOut,
    Unavailable,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(((sorted.len() - 1) as f64) * p).round() as usize]
}

impl Stats {
    pub fn new() -> Self {
        Stats { started: Instant::now(), inner: Mutex::new(Inner::default()) }
    }

    pub fn record(&self, tool: &str, test_mode: bool, ok: bool, ms: u64) {
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut g = self.inner.lock().unwrap();
        let t = g.tools.entry(tool.to_string()).or_default();
        if test_mode { t.test += 1 } else { t.real += 1 }
        if !ok {
            t.errors += 1;
        }
        if t.ms.len() >= KEEP {
            t.ms.remove(0);
        }
        t.ms.push(ms);
        t.last = Some(ts.clone());
        g.last_call = Some(json!({ "tool": tool, "ok": ok, "ms": ms, "test_mode": test_mode, "at": ts }));
    }

    /// Une requête vers autre chose que le mock a été coupée par le coupe-circuit.
    pub fn egress_blocked(&self) {
        self.inner.lock().unwrap().egress_blocked += 1;
    }

    /// Une écriture (POST/PUT/PATCH/DELETE) a été exécutée par le mock : jamais par NetBox.
    pub fn mock_write(&self) {
        self.inner.lock().unwrap().mock_writes += 1;
    }

    pub fn validation(&self, v: Validation) {
        let mut g = self.inner.lock().unwrap();
        match v {
            Validation::Approved => g.approved += 1,
            Validation::Denied => g.denied += 1,
            Validation::TimedOut => g.timed_out += 1,
            Validation::Unavailable => g.no_confirmation += 1,
        }
    }

    pub fn snapshot(&self) -> Value {
        let g = self.inner.lock().unwrap();
        let (mut real, mut test, mut errors) = (0, 0, 0);
        let mut tools = Map::new();
        for (name, t) in &g.tools {
            let mut s = t.ms.clone();
            s.sort_unstable();
            real += t.real;
            test += t.test;
            errors += t.errors;
            tools.insert(name.clone(), json!({ "calls": t.real + t.test, "real": t.real, "test_mode": t.test, "errors": t.errors, "p50_ms": percentile(&s, 0.5), "p95_ms": percentile(&s, 0.95), "last_call_at": t.last }));
        }
        json!({
            "uptime_s": self.started.elapsed().as_secs(),
            "calls": real + test,
            "real_calls": real,
            "test_mode_calls": test,
            "errors": errors,
            "egress_blocked": g.egress_blocked,
            "mock_writes": g.mock_writes,
            "validations": { "approved": g.approved, "denied": g.denied, "timed_out": g.timed_out, "no_confirmation_possible": g.no_confirmation },
            "last_call": g.last_call,
            "tools": tools,
        })
    }

    /// Écrit l'instantané dans un fichier local (au mieux : une erreur d'écriture ne fait jamais échouer un appel).
    pub fn export(&self, path: &str) {
        if let Err(e) = std::fs::write(path, self.snapshot().to_string()) {
            crate::log::error(&format!("could not write NETBOX_STATS_FILE: {e}"));
        }
    }
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_percentiles_and_split() {
        let s = Stats::new();
        for ms in 1..=100 {
            s.record("a", true, true, ms);
        }
        s.record("a", false, false, 5);
        s.egress_blocked();
        s.validation(Validation::Approved);
        let v = s.snapshot();
        assert_eq!(v["tools"]["a"]["calls"], 101);
        assert_eq!(v["tools"]["a"]["errors"], 1);
        assert_eq!((v["real_calls"].as_u64(), v["test_mode_calls"].as_u64()), (Some(1), Some(100)));
        assert_eq!(v["tools"]["a"]["p95_ms"], 95);
        assert_eq!(v["egress_blocked"], 1);
        assert_eq!(v["validations"]["approved"], 1);
    }
}
