# 🦀 nbmcp — NetBox MCP en Rust (fork)

**Un serveur MCP pour NetBox, en un seul binaire Rust. Il lit, et il écrit, sous contrôle.**
Fork de `netbox-mcp-server` (Apache-2.0). L'upstream est en lecture seule : ici on a ajouté toute l'écriture et les plugins.

## ✨ Ce que ça apporte
• **Lecture** : mêmes outils que l'upstream, en plus rapide, plus `describe_type`, GraphQL, trace de câble, IP/préfixes libres, statut
• **Écriture** : créer, modifier, supprimer (un ou en lot), allouer la prochaine IP/préfixe/VLAN/ASN, lancer un script
• **Plugins** : leurs modèles sont découverts tout seuls et marchent avec les mêmes outils (`netbox_dns.zone`…) ; `api_get/write/delete` pour le reste
• **Un seul binaire**, pas de Python

## 🔒 Sécurité (l'écriture est coupée par défaut)
• `NETBOX_ALLOW_WRITES=true` pour écrire, `NETBOX_ALLOW_DELETE=true` en plus pour supprimer / scripts
• `NETBOX_DRY_RUN=true` : tout est simulé, rien n'est envoyé
• Types autorisés/interdits (`NETBOX_WRITE_TYPES`, `NETBOX_WRITE_DENY`), confirmation humaine, journal d'audit, secrets masqués
• Un refus arrive **avant** toute requête

## 🐛 Bugs corrigés par rapport à l'upstream
• Les opérations en lot visaient un chemin `/bulk/` qui n'existe pas
• La recherche répondait « aucun résultat » quand le jeton était refusé
• Pas de repli pour l'historique sur NetBox < 4.1
• Les filtres `__in` / multi-sauts renvoyaient des données fausses sans prévenir

## 🚀 Démarrer
```
cd rust && cargo build --release
NETBOX_URL=https://netbox.exemple.fr NETBOX_TOKEN=xxx ./target/release/nbmcp
```
Option HTTP : `--transport http --port 8000` (jeton par client possible avec `NETBOX_TOKEN_PASSTHROUGH=true`).

📖 Doc complète : `rust/README.md` · 🧪 `cargo test` (unitaires + test de bout en bout contre un faux NetBox)
⚠️ Pas encore éprouvé sur un vrai NetBox.
