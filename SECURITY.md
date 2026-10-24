# Sécurité de myMoney

## Ce qui est chiffré

| Donnée | Protection |
|---|---|
| Base de données (comptes, titulaires, adresses, IBAN et RIB, opérations, bénéficiaires et contacts, positions) | SQLCipher 4, AES-256 sur chaque page, HMAC, fichiers WAL inclus |
| Documents importés (PDF, CSV…) | AES-256-GCM, un fichier par document, nom aléatoire (le vrai nom est dans la base chiffrée) ; déchiffrés à la volée au téléchargement |
| Clé maîtresse | Aléatoire (256 bits), stockée uniquement chiffrée dans `keystore.json` |

Dérivation des clés :

```
mot de passe de chaque utilisateur ──Argon2id (128 Mio, 3 passes, sel aléatoire)──► KEK ──AES-GCM──► clé maîtresse (DEK)
                                       (un « emplacement de clé » par utilisateur, nommé par un hachage de son e-mail)
code de secours ──Argon2id──► KEK de secours ──AES-GCM──► même DEK
DEK ──HKDF-SHA256──► clé base / clé documents / clé secrets
```

- Le mot de passe et la clé maîtresse ne sont **jamais** écrits sur le disque.
- Changer de mot de passe ré-enveloppe la clé de **cet** utilisateur sans re-chiffrer la base ni toucher aux autres ; supprimer un utilisateur supprime son emplacement.
- Le code de secours (160 bits d'entropie) n'est affiché qu'une fois.

## Plusieurs utilisateurs

- Une seule clé de données pour tout le coffre. La séparation entre utilisateurs (propriétaire d'un compte, partages en lecture ou modification, documents) est faite **par l'application, à chaque requête**, pas par le chiffrement : quelqu'un qui a le coffre ouvert au niveau du serveur (administrateur du NAS, root pendant que l'app tourne) peut tout lire.
- Un compte qu'on ne peut pas voir n'existe pas pour soi (réponse « introuvable »). Seul le propriétaire supprime un compte ou change ses partages ; un partage en lecture seule ne permet ni modification, ni import, ni suppression de document.
- **Cloudflare Access** : un visiteur n'est reconnu que par le **jeton signé** `Cf-Access-Jwt-Assertion`, vérifié (RS256 uniquement, clés publiques de Cloudflare, `aud` de ton application, `iss` de ton équipe, expiration). Les en-têtes d'identité en clair sont ignorés : sur le réseau local n'importe qui peut en envoyer. Sans configuration (`MYMONEY_CF_TEAM_DOMAIN` et `MYMONEY_CF_AUD`), les jetons sont ignorés. Sans jeton valide, il reste e-mail et mot de passe, avec le même ralentissement après des échecs.
- Le jeton ouvre une session **seulement si le coffre est déjà ouvert** : Cloudflare ne connaît aucun mot de passe et ne peut donc pas déchiffrer quoi que ce soit.
- Ne laisse **pas** le port de l'application joignable depuis Internet : tout doit passer par le tunnel (et donc par Access). Le tunnel est sortant, aucune redirection de port n'est nécessaire.

## Coffre verrouillé ou ouvert

- **Verrouillé** au démarrage du conteneur, sur clic « Verrouiller » (réservé aux administrateurs : il ferme le coffre pour tout le monde), et après `MYMONEY_AUTO_LOCK_MINUTES` minutes sans session web active (`0` : jamais). La clé est alors effacée de la mémoire et la base fermée : le contenu de `/data` est illisible, même pour root sur le NAS.
- **Ouvert** : la clé est en mémoire du processus. Un attaquant root sur le NAS à ce moment-là pourrait la lire. Verrouille si tu n'utilises pas l'app.

### Déverrouillage automatique (option, déconseillé)

`MYMONEY_UNLOCK_PASSWORD_FILE` (secret Docker, avec `MYMONEY_UNLOCK_EMAIL` : l'e-mail de l'utilisateur dont c'est le mot de passe) évite de saisir le mot de passe au démarrage, mais **le mot de passe est alors sur le NAS** : quiconque accède aux fichiers du NAS peut tout déchiffrer. Le verrouillage automatique est désactivé dans ce mode. N'utilise cette option que si le NAS est lui-même chiffré et bien protégé.

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
- **L'application ne se connecte à aucune banque** et ne stocke aucun identifiant bancaire : elle ne lit que les documents que tu déposes. Ils sont analysés **localement** (aucun service externe, aucune IA en ligne).
- Seuls appels sortants : les cotations des positions saisies à la main (CoinGecko, Yahoo Finance, BCE). Ils ne contiennent ni nom, ni IBAN, ni montant de compte.

## Données personnelles et dépôt Git

- Les relevés contiennent nom, adresse, RIB et noms de bénéficiaires : tout cela n'existe que dans la base chiffrée et dans les fichiers chiffrés de `data/`.
- Le dépôt ne contient aucun document réel. Les tests fabriquent des relevés **fictifs** (`tests/fixtures.py`). `.gitignore` écarte les PDF, CSV, OFX, QFX et QIF et le dossier `samples/` pour éviter un commit par mégarde : garde tes vrais relevés hors du dépôt, et ne les joint pas aux issues.

## Limites connues

- Python ne garantit pas l'effacement complet des copies d'une clé en mémoire : l'effacement au verrouillage est au mieux.
- La suppression d'un document écrase le fichier chiffré avant de l'effacer, mais les SSD et les systèmes copy-on-write (Btrfs sur Synology) peuvent garder d'anciens blocs. Ils restent toutefois chiffrés.
- Les sauvegardes de `/data` sont chiffrées ; perdre le mot de passe **et** le code de secours rend les données irrécupérables.

## Signaler un problème

Ouvre une issue privée (security advisory) sur le dépôt GitHub.
