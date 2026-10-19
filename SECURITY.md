# Sécurité de myMoney

## Ce qui est chiffré

| Donnée | Protection |
|---|---|
| Base de données (comptes, opérations, positions, IBAN…) | SQLCipher 4, AES-256 sur chaque page, HMAC, fichiers WAL inclus |
| Documents importés (PDF, CSV…) | AES-256-GCM, un fichier par document, nom aléatoire |
| Identifiants bancaires, clés API, sessions, cookies | AES-256-GCM avec une sous-clé dédiée, **en plus** du chiffrement de la base |
| Clé maîtresse | Aléatoire (256 bits), stockée uniquement chiffrée dans `keystore.json` |

Dérivation des clés :

```
mot de passe ──Argon2id (128 Mio, 3 passes, sel aléatoire)──► KEK ──AES-GCM──► clé maîtresse (DEK)
code de secours ──Argon2id──► KEK de secours ──AES-GCM──► même DEK
DEK ──HKDF-SHA256──► clé base / clé documents / clé secrets
```

- Le mot de passe et la clé maîtresse ne sont **jamais** écrits sur le disque.
- Changer de mot de passe ré-enveloppe la clé sans re-chiffrer la base.
- Le code de secours (160 bits d'entropie) n'est affiché qu'une fois.

## Coffre verrouillé ou ouvert

- **Verrouillé** au démarrage du conteneur, sur clic « Verrouiller », et après `MYMONEY_AUTO_LOCK_MINUTES` minutes sans session web active. La clé est alors effacée de la mémoire et la base fermée : le contenu de `/data` est illisible, même pour root sur le NAS.
- **Ouvert** : la clé est en mémoire du processus. Un attaquant root sur le NAS à ce moment-là pourrait la lire. Verrouille si tu n'utilises pas l'app.
- La synchro automatique ne tourne que coffre ouvert.

### Déverrouillage automatique (option, déconseillé)

`MYMONEY_UNLOCK_PASSWORD_FILE` (secret Docker) permet la synchro sans intervention, mais **le mot de passe est alors sur le NAS** : quiconque accède aux fichiers du NAS peut tout déchiffrer. Le verrouillage automatique est désactivé dans ce mode. N'utilise cette option que si le NAS est lui-même chiffré et bien protégé.

## Interface web

- Code d'installation à usage unique (logs du conteneur) : personne sur ton réseau ne peut créer le coffre avant toi.
- Cookie de session aléatoire, `HttpOnly`, `SameSite=Strict`, `Secure` en HTTPS. Les sessions vivent en mémoire uniquement.
- Jeton CSRF sur chaque formulaire, vérification de l'en-tête `Origin`.
- Délai exponentiel après 3 mots de passe faux (jusqu'à 5 min).
- `Content-Security-Policy` stricte (`default-src 'none'`), aucun script, police ou image externe, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`.
- Les secrets ne sont jamais renvoyés au navigateur (champ vide = inchangé).
- Pas de documentation d'API exposée, pas d'en-tête `Server`, logs HTTP désactivés.

## Conteneur

Utilisateur non root (uid 10001), système de fichiers en lecture seule, `/tmp` en RAM, `cap_drop: ALL`, `no-new-privileges`, limites mémoire et processus, fichiers créés en mode 600.

## Réseau et tiers

- **N'expose pas l'app sur Internet.** Utilise un VPN (Tailscale, WireGuard) pour un accès à distance.
- Active HTTPS via le reverse proxy du NAS puis `MYMONEY_COOKIE_SECURE=true`.
- Appels sortants : banques et agrégateurs que tu configures, plateformes crypto, API publiques de blockchain (Blockscout, mempool.space, RPC publics), CoinGecko, Yahoo Finance, BCE. Ces services voient l'IP du NAS et, pour les blockchains, les adresses consultées. Pour plus de confidentialité, pointe les connecteurs vers ton propre nœud.
- Clés API des plateformes : **lecture seule**, sans trading ni retrait, restreintes à l'IP de ta box si possible.
- Wallets : uniquement des adresses ou xpub publiques. **Aucune seed phrase ni clé privée n'est jamais demandée.**

## Limites connues

- Python ne garantit pas l'effacement complet des copies d'une clé en mémoire : l'effacement au verrouillage est au mieux.
- La suppression d'un document écrase le fichier chiffré avant de l'effacer, mais les SSD et les systèmes copy-on-write (Btrfs sur Synology) peuvent garder d'anciens blocs. Ils restent toutefois chiffrés.
- Woob et Trade Republic utilisent des identifiants web : ils sont chiffrés, mais transmis au site de la banque à chaque synchro (comme dans un navigateur).
- Les sauvegardes de `/data` sont chiffrées ; perdre le mot de passe **et** le code de secours rend les données irrécupérables.

## Signaler un problème

Ouvre une issue privée (security advisory) sur le dépôt GitHub.
