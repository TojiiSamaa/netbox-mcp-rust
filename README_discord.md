# 🦀 nbmcp — serveur MCP NetBox en Rust

**Un seul binaire. Il lit NetBox, et il écrit, sous contrôle.**
Fork de [`netboxlabs/netbox-mcp-server`](https://github.com/netboxlabs/netbox-mcp-server) (Apache-2.0). L'original est en lecture seule ; ici toute l'écriture et les plugins sont ajoutés.

📦 **Dépôt** : https://github.com/TojiiSamaa/netbox-mcp-server (code Rust dans `rust/`)

## ✨ Fonctions
• **Lecture** : les mêmes outils que l'original, plus description d'un type (champs, choix, filtres), statut, GraphQL, trace de câble, IP/préfixes/VLAN/ASN libres, élévation de baie
• **Écriture** : créer, modifier, supprimer (un objet ou un lot), allouer la prochaine IP/préfixe/VLAN/ASN, lancer un script
• **Plugins** : leurs modèles sont découverts tout seuls et marchent avec les mêmes outils (`netbox_dns.zone`…) ; `api_get` / `api_write` / `api_delete` pour tout le reste
• **Deux modes** : stdio, ou HTTP avec un jeton NetBox par client

## 🔒 Sécurité
• L'écriture est **coupée par défaut** : `NETBOX_ALLOW_WRITES=true` pour écrire, `NETBOX_ALLOW_DELETE=true` en plus pour supprimer et lancer des scripts
• `NETBOX_DRY_RUN=true` : tout est simulé, rien n'est envoyé
• Types autorisés/interdits (`NETBOX_WRITE_TYPES`, `NETBOX_WRITE_DENY`), confirmation humaine, journal d'audit, secrets masqués
• Un refus arrive **avant** toute requête ; les permissions du jeton NetBox s'appliquent en plus

## 🐛 Bugs de l'original corrigés
• Les lots visaient un chemin `/bulk/` qui n'existe pas
• La recherche répondait « aucun résultat » quand le jeton était refusé
• L'historique n'avait pas de repli sur NetBox < 4.1
• Les filtres `__in` et multi-sauts renvoyaient des données fausses sans prévenir

## 🚀 Démarrer
```
cd rust && cargo build --release
NETBOX_URL=https://netbox.exemple.fr NETBOX_TOKEN=xxx ./target/release/nbmcp
```
HTTP : `./nbmcp --transport http --port 8000` (endpoint `/mcp`).

## 🧪 État
• `cargo test` : 31 tests unitaires + 12 tests de bout en bout contre un faux NetBox, tous verts
• ⚠️ Pas encore éprouvé sur un vrai NetBox

📖 Doc complète : `README.md`
