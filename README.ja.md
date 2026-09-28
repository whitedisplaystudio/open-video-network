# Open Video Network

中心のない動画ネットワーク。

ネットワークは動画を保持する。あなたの端末は、あなたのことを知っている。
分散システムの複雑さはソフトウェアが引き受ける。

英語版は [README.md](README.md)、プロトコル仕様は
[protocol/SPECIFICATION.md](protocol/SPECIFICATION.md) にあります。

> **状態: V1 ドラフト。** 設計書 §37 の受け入れテスト A〜H が通っています
> （[`crates/node/tests/`](crates/node/tests/)）。「開発者が運用するものを
> すべて停止してもネットワークが動き続ける」ことを含みます。

---

## 3つの原則

詳細は [PRINCIPLES.md](PRINCIPLES.md) にあります。

1. **中央サーバーを持たない。** 開発者が動かしている機器をすべて止めても、
   既存 Peer だけでネットワークは存続します。必須の Bootstrap Node も、
   必須の Domain もありません。
2. **Recommendation は端末内で計算する。** 視聴履歴、視聴時間、視聴率、
   スキップ、Preference Vector、Recommendation Score は端末から出ません。
   「送信を OFF にできる」ではなく、**送信する仕組み自体がありません**。
   視聴データを保持する型には Serialize の実装がなく、
   [テストで保証されています](crates/node/tests/privacy.rs)。
3. **複雑性はソフトウェアが引き受ける。** Peer ID も Multiaddr も CID も
   公開鍵も入力しません。Node 起動は1コマンド、ネットワーク参加はリンク1つです。

---

## クイックスタート

### 必要なもの

* Rust 1.85 以降
* それだけです。FFmpeg は任意で、動画の長さを読むためだけに使います。

### ビルドと起動

```bash
cargo build --release
./target/release/ourvideo start
```

初回起動時に、何も聞かずに次を実行します。

1. Ed25519 の Identity を生成し、`0600` で保存
2. Data Directory の作成
3. SQLite の初期化とマイグレーション
4. QUIC / TCP での待ち受け開始
5. mDNS でローカルネットワークの Peer を探索
6. DHT と GossipSub への参加
7. CLI や将来の GUI のための Local API を `127.0.0.1` で起動

起動すると共有リンクが表示されます。そのリンクがあれば誰でもあなたに接続できます。

### 画面を開く

```bash
ourvideo ui            # 視聴者用
ourvideo ui --admin    # 管理用
```

どちらも Node 自身が loopback で配信します。別の Web サーバーもビルド手順も
JavaScript のツールチェーンも不要で、`cargo build` だけで完結します。

### ネットワークに参加する

別のターミナルで:

```bash
ourvideo peer add ourvideo://…              # 共有リンク
ourvideo peer add https://video.example.jp  # または URL
```

入力するのは文字列1つだけです。リンクは**入口**であって依存先ではありません。
一度 Peer を見つけたあとは DHT で他の Peer を発見するので、そのリンクが
消滅してもネットワーク参加は継続できます。

### 投稿と視聴

```bash
ourvideo video publish holiday.mp4 --title "旅行" --tag travel --tag family
ourvideo video list
ourvideo video get <CID>          # P2P で取得して再生可能なファイルを保存
                                  # （UI ではダウンロードせずストリーミング再生します）
ourvideo search 旅行               # ローカル検索。クエリは端末から出ません
ourvideo watch <CID> --seconds 120 --completed
ourvideo recommendation list      # あなた専用のフィード（端末内で計算）
ourvideo recommendation explain <CID>   # なぜその順位なのか
```

---

## 主なコマンド

| コマンド | 内容 |
| --- | --- |
| `ourvideo start` | Node を起動（他のコマンドはこれが必要） |
| `ourvideo status` | Peer・動画・キャッシュ・待ち受けアドレス |
| `ourvideo stop` | Node を停止 |
| `ourvideo share-link` | 自分に接続してもらうためのリンク |
| `ourvideo ui [--admin]` | Web 画面をブラウザで開く |

`start` には `--locale ja` で表示言語を固定できます（環境変数 `OURVIDEO_LOCALE` も可）。
`--ui-auth none` を付けると、Web 画面がサインインなしの固定 URL で開けます（下記の注意点を参照）。
| `ourvideo peer list` / `add` / `remove` | Peer の一覧・追加・削除 |
| `ourvideo video publish <FILE>` | Chunk 分割・CID 化・署名・Announcement |
| `ourvideo video list [--local]` | 発見済み動画 / 自分が投稿した動画 |
| `ourvideo video info <CID>` | 1本の詳細 |
| `ourvideo video get <CID>` | データ取得と書き出し |
| `ourvideo search <QUERY>` | ローカル全文検索（FTS5） |
| `ourvideo recommendation list` / `explain <CID>` | フィード / その理由 |
| `ourvideo watch <CID> --seconds N` | 視聴記録（端末内のみ） |
| `ourvideo privacy show` / `preferences` / `clear` | 端末内データの確認と消去 |
| `ourvideo block cid` / `creator` / `list` | ローカルでの非表示 |
| `ourvideo profile --name "…"` | 表示名の公開 |
| `ourvideo follow <KEY>` | Creator のフォロー |

どのコマンドにも `--json`（生のレスポンス）と `--data-dir PATH` を付けられます。

### ポート

既定は `4800`（UDP/TCP、P2P）と `4801`（TCP、ループバック限定、Local API）です。
80 / 443 / 3000 / 5000 / 5432 / 6379 / 8080 といった衝突しやすいポートは避けています。
念のため確認してください。

```bash
lsof -i :4800
lsof -i :4801
```

同じマシンで2つ目の Node を動かす場合は、ポートとディレクトリを分けます。

```bash
ourvideo --data-dir ~/.ovn-second start --port 4810 --api-port 4811
```

### データの置き場所

既定は OS ごとのユーザーデータ領域
（macOS なら `~/Library/Application Support/network.OpenVideoNetwork.ourvideo`）です。

```
identity.key    Ed25519 秘密鍵（0600）。これがあなたの Identity です。要バックアップ
node.db         SQLite。Peer・発見した動画・キャッシュ管理、
                そして端末から出ない視聴データ
blocks/         コンテンツ。1 Block 1 ファイル、ファイル名は Content ID
downloads/      `video get` の既定の書き出し先
uploads/        ブラウザからのアップロードの一時置き場。Chunk 化後すぐ削除されます
locales/        言語パックの置き場。翻訳の追加・修正はここに JSON を置くだけです
runtime.json    CLI が起動中の Node を見つけるための情報（API トークンを含むため 0600）
api.token       Local API のトークン（0600）。再起動してもブラウザのログイン状態が
                続くよう保存されます。削除して再起動すれば全セッションを無効化できます
```

---

## Web 画面

Node が2つのページを配信します。どちらも CLI と同じ Local API を使うクライアントです。
`ourvideo ui` で開くと、トークンが一度だけ `HttpOnly` / `SameSite=Strict` の Cookie に
交換されます（ブラウザは `<video src>` や `EventSource` に Authorization ヘッダを
付けられないためです）。

**この操作はブラウザごとに一度だけです。** 以降は `http://127.0.0.1:4801/ui` を
ブックマークすれば、ノードを再起動しても使い続けられます（トークンはデータ
ディレクトリに保存されます）。全セッションを無効化したいときは `api.token` を
削除して再起動してください。

### サインイン手順ごと省く

共有していないマシンなら、トークンを外して「固定 URL を開くだけ」にできます。

```bash
ourvideo start --ui-auth none
```

引き換えに失うもの: loopback 束縛と `Host` 検査は残るのでネットワークや他サイトからは
届きませんが、**同じマシンの別アカウントがノードを操作し、視聴履歴を読めるようになります**。
トークンが守っていたのはまさにこれだけで、だからこそ既定は `token` です — 視聴履歴は
このプロジェクトが「外に出さない」と約束しているデータそのものだからです。

**視聴者用 `/ui`** — 端末内で計算したフィード、ブラウズ、ローカル検索、そして
**ストリーミング再生**。プレイヤーが要求した分だけ Chunk を Peer から取得するので、
全部ダウンロードし終わる前に再生が始まります。`Range` に対応しているのでシークもできます。
各レコメンドは「なぜこの順位なのか」の内訳を展開できます。

どちらも 日本語・English・Español・Português・العربية に対応します。表示言語は
端末内の情報だけから決めます — 前回の選択、`--locale` での指定、ブラウザの言語設定、
それでも決まらなければブラウザのタイムゾーンから国を判定します。**IP による位置判定は
行いません** — 第三者に居場所と「このソフトを使っている事実」を渡すことになりますし、
ブラウザは自分のタイムゾーンをすでに知っているので不要です。**言語は JSON ファイル1つ**です。Node の `locales`
ディレクトリに置けば、次回の再読み込みで言語選択に現れます — 再ビルドも再起動も
登録作業も不要です。右から左に書く言語は `"direction": "rtl"` を指定するだけで、
レイアウト全体が反転します（[docs/TRANSLATING.md](docs/TRANSLATING.md)）。

**管理用 `/admin`** — ステータス、Peer 一覧とリンクからの参加、動画の公開
（ファイルはディスクへストリーミングしてから Chunk 化するので、メモリには載りません）、
ストレージとキャッシュ、ローカル Moderation、そして端末が記録しているあなたのデータの
表示と消去。

両方ともイベントストリームから進捗をリアルタイム表示します（Peer の増減、
Announcement の到着、ダウンロードの進捗バー）。

ページは厳格な `Content-Security-Policy` の下で配信され、この Node 以外からは
何も読み込みません。また `Host` が loopback 名でないリクエストは拒否するので、
DNS rebinding も塞いでいます。

サムネイルは FFmpeg があれば生成し、通常のコンテンツ Block として保存・P2P 配信します。
FFmpeg が無い場合はサムネイルが付かないだけで、公開は成功します。

---

## 開発

```bash
cargo test --workspace      # 312 テスト（受け入れテスト A〜H を含む）
cargo clippy --workspace --all-targets
cargo fmt --all
```

`crates/node/tests/acceptance.rs` の受け入れテストは、実際の Node
（実 Identity・実 SQLite・実 libp2p Swarm、ループバックの空きポート）を
プロセス内に複数立ち上げて検証します。テスト専用の環境も専用 DB も作らず、
`ourvideo start` と同じコードパスを通します。

### うまくいかないとき

**macOS で `cc` が「You have not agreed to the Xcode license agreements」で失敗する**

一度ライセンスに同意するか:

```bash
sudo xcodebuild -license accept
```

Command Line Tools を使うように指定します（同意不要）:

```bash
export DEVELOPER_DIR=/Library/Developer/CommandLineTools
```

**`the local API could not bind to 127.0.0.1:4801`** — すでに Node が
起動しています。`ourvideo status` で確認するか、別ポートで起動してください。

---

## V1 で対象外のこと

設計書に明記されている既知の制約です。

* **動画の永続性は保証しません。** Creator がオフラインで、どの Peer の
  キャッシュからも消えた動画は失われます（Persistent Node / NAS Node は V1.5 候補）。
* **Transcoding と Adaptive Streaming はありません。** 投稿された元ファイルを配信します。
* **画質の切り替え（Adaptive Bitrate）はありません。** 元ファイルをそのまま
  ストリーミングするため、回線が細いと解像度が下がるのではなくバッファリングします。
* **Moderation はローカルのみです。** 署名付き Moderation List は V1.5 候補です。
* **NAT traversal は基本的なものだけです。** Relay や Hole punching はまだありません。
* **CLI は英語のみです。** Web 画面は多言語化していますが、CLI の出力とヘルプは未対応です。

---

## ガバナンスとライセンス

BDFL 方式です（[GOVERNANCE.md](GOVERNANCE.md)）。コントリビュートは
[CONTRIBUTING.md](CONTRIBUTING.md) と
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) をご覧ください。翻訳が一番はじめやすい
入口です（[docs/TRANSLATING.md](docs/TRANSLATING.md)）。

ライセンスは [Apache-2.0](LICENSE-APACHE) または [MIT](LICENSE-MIT) の
デュアルライセンスです。設計書ではライセンスが TBD のため、これは Rust
エコシステムの慣例に沿った暫定の既定値で、最初のリリースまでに BDFL が
変更する可能性があります。
