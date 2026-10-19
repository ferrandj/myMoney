# myMoney

Gestionnaire de patrimoine personnel **auto-hébergé** (NAS, Docker) : comptes bancaires, livrets, assurance-vie, bourse, crypto (plateformes et wallets), immobilier. Tout est **chiffré au repos**, rien ne part chez un tiers en dehors des sources de données que tu configures.

## Fonctionnalités

- **Patrimoine net** avec répartition (liquidités, épargne, investissements, crypto, immobilier, dettes) et historique.
- **Connecteurs gratuits** synchronisés automatiquement (toutes les 6 h quand le coffre est ouvert) :

| Connecteur | Pour | Comment |
|---|---|---|
| Enable Banking (DSP2) | Fortuneo, BforBank, Hello bank!, Boursorama, Revolut, espèces Trade Republic… | API officielle d'open banking, gratuite pour tes propres comptes (mode restreint) |
| GoCardless Bank Account Data | idem, si tu as déjà un compte | API officielle, offre gratuite |
| Woob | assurance-vie, PEA, compte-titres, PER, épargne salariale, banques sans DSP2 | bibliothèque libre qui lit le site de la banque |
| Trade Republic (pytr) | portefeuille actions, ETF, crypto et espèces | client libre de l'app web, validation dans l'app mobile |
| Plateformes crypto (ccxt) | Binance (y compris Earn et Funding), Kraken (y compris staking), Coinbase, Bitstamp, Crypto.com, KuCoin, Bybit, OKX… | clés API en lecture seule |
| Wallets EVM | Ethereum, Base, Arbitrum, Optimism, Polygon, BNB Chain, Avalanche, Gnosis, Linea, Scroll, zkSync, Celo | adresses publiques, RPC publics et Blockscout, filtre anti-spam |
| Wallet Bitcoin | adresses, xpub, ypub ou zpub (scan avec limite de 20 adresses vides) | mempool.space ou ton propre mempool |
| Wallet Solana | SOL et tokens SPL | RPC public ou le tien |

- **Prix gratuits** : CoinGecko (crypto, y compris par adresse de contrat), Yahoo Finance (actions, ETF par ISIN), BCE via Frankfurter (devises).
- **Import de documents** : relevés **PDF** (colonnes débit et crédit détectées, contrôle par les soldes), **CSV** (toutes banques, Revolut, listes de positions), **OFX/QFX**, **QIF**. Les comptes peuvent être **créés à partir des documents** (IBAN, établissement et type détectés). Rien n'est écrit sans ta validation. Les originaux sont conservés chiffrés.
- **Taux réglementés automatiques** : Livret A, LDDS, LEP, CEL, Livret Jeune. Le taux en vigueur à la date du jour est appliqué (1,70 % pour le Livret A depuis le 01/08/2026). Un taux saisi sur le compte (ex. livret boosté Cashbee) reste prioritaire.
- **Comptes manuels** : livret, bien immobilier, crédit, positions crypto ou bourse saisies à la main (Nexo, Revolut crypto…), valorisées automatiquement.

## Sécurité

Résumé (détails dans [SECURITY.md](SECURITY.md)) :

- **Base de données chiffrée** (SQLCipher, AES-256), **documents chiffrés** (AES-256-GCM), identifiants et clés API **chiffrés une seconde fois**.
- La clé maîtresse est aléatoire. Elle n'est stockée que **chiffrée** par ton mot de passe (Argon2id) et par un **code de secours** affiché une seule fois. **Rien n'est lisible sur le NAS quand le coffre est verrouillé** (au démarrage, sur demande ou après 15 min d'inactivité).
- Interface web durcie : CSP stricte sans aucune ressource externe, cookies HttpOnly et SameSite=Strict, jetons CSRF, anti-force brute, code d'installation à usage unique.
- Conteneur durci : utilisateur non root, système de fichiers en lecture seule, aucune capability, `no-new-privileges`.

> ⚠️ **Sans le mot de passe ni le code de secours, les données sont irrécupérables.** C'est voulu.

## Installation sur le NAS

1. Copie `docker-compose.yml` dans un dossier du NAS (ex. `/volume1/docker/mymoney/`).
2. Construis l'image ou récupère-la :
   - `docker compose build` après avoir décommenté `build: .` (avec le code source dans le dossier), **ou**
   - l'image `ghcr.io/ferrandj/mymoney` publiée par la CI GitHub (amd64 et arm64).
3. Adapte `MYMONEY_PUBLIC_URL` puis lance `docker compose up -d`.
4. Récupère le code d'installation : `docker logs mymoney`.
5. Ouvre `http://IP-DU-NAS:8000`, crée ton mot de passe maître et **note le code de secours**.

Sur Synology : Container Manager → Projet → Créer → choisir le dossier qui contient `docker-compose.yml`.

### Accès à distance

N'ouvre **pas** le port sur Internet. Utilise un VPN (Tailscale ou WireGuard, souvent disponibles sur le NAS). Pour le HTTPS en local, place l'app derrière le reverse proxy du NAS et passe `MYMONEY_COOKIE_SECURE=true`.

### Sauvegardes

Sauvegarde le dossier `data/` tel quel (Hyper Backup, rsync…). Il est chiffré : la sauvegarde l'est aussi. Garde le code de secours **ailleurs** que sur le NAS.

## Ajouter tes comptes

- **Banques** (Fortuneo, BforBank, Hello bank!…) : Connexions → Enable Banking. Le mode d'emploi s'affiche dans le formulaire. Une connexion par banque. Le consentement dure 180 jours.
- **Assurance-vie, PEA chez un assureur ou un courtier** : Connexions → Woob, ou import du relevé de situation PDF.
- **Trade Republic** : Connexions → Trade Republic (numéro et PIN, puis validation dans l'app).
- **Binance, Kraken** : Connexions → Plateforme crypto, avec une clé API **en lecture seule**.
- **Revolut crypto, Nexo, Cashbee** : pas d'API gratuite. Importe leurs exports CSV ou PDF, ou crée un compte manuel avec tes positions.
- **Wallets** : colle tes adresses publiques ou ton xpub. **Jamais de seed phrase.**

## Développement

```bash
python -m venv .venv && . .venv/bin/activate
pip install -e ".[dev,woob,traderepublic]"
pytest
MYMONEY_DATA_DIR=./data mymoney serve   # http://127.0.0.1:8000
```

Structure :

```
mymoney/
  security/    crypto (AES-GCM, Argon2id, HKDF), trousseau de clés, coffre en mémoire
  db.py        base SQLCipher ouverte uniquement quand le coffre est déverrouillé
  connectors/  banques, plateformes, wallets
  importers/   PDF, CSV, OFX, QIF
  services/    prix, valorisation, taux réglementés, synchronisation
  web/         interface (FastAPI + Jinja, sans JavaScript externe)
```

Les taux réglementés se mettent à jour dans `mymoney/services/rates.py` (une ligne par changement, les 1er février et 1er août).
