#!/bin/sh
# Mutation : ajoute un appel réseau réel HORS du point unique (client.rs) ; les preuves A (statique) et B (binaire,
# écouteur sentinelle) doivent ÉCHOUER. Le fichier est restauré dans tous les cas.
set -u
cd "$(dirname "$0")/.."
cp src/tools.rs /tmp/tools.rs.orig
trap 'cp /tmp/tools.rs.orig src/tools.rs; echo "[mutation] tools.rs restauré"' EXIT
sed -i '/^fn get_status/a\    let _ = std::net::TcpStream::connect(std::env::var("NETBOX_URL").unwrap_or_default().trim_start_matches("http://"));' src/tools.rs
grep -q 'TcpStream::connect' src/tools.rs || { echo "injection impossible"; exit 2; }
fail=0
echo "[mutation] preuve A (statique) doit échouer :"
if cargo test --test egress_static 2>&1 | tail -15 | tee /tmp/mA.log | grep -q "test result: FAILED"; then echo "  A a échoué (attendu)"; else echo "  A N'A PAS ÉCHOUÉ"; fail=1; fi
echo "[mutation] preuve B (binaire + sentinelle) doit échouer :"
if cargo test --test test_mode proof_b_binary 2>&1 | tail -15 | tee /tmp/mB.log | grep -q "test result: FAILED"; then echo "  B a échoué (attendu)"; else echo "  B N'A PAS ÉCHOUÉ"; fail=1; fi
tail -8 /tmp/mA.log /tmp/mB.log
exit $fail
