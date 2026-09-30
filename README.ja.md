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

### インストール

[リリースページ](../../releases)から自分の環境に合うものをダウンロードし、展開して
`ourvideo` を実行するだけです。Web 画面も言語パックもバイナリに同梱されているので、
**ファイル1つで完結します**。

| ファイル | 対象 |
| --- | --- |
| `ourvideo-macos-arm64.tar.gz` | Apple シリコンの Mac |
| `ourvideo-macos-x86_64.tar.gz` | Intel Mac |
| `ourvideo-linux-x86_64.tar.gz` | 一般的な Linux |
| `ourvideo-windows-x86_64.zip` | Windows |

**バイナリはコード署名していないため、「開発元が不明」という警告が出ます。**
署名するには Apple と証明書発行元に毎年料金を払う必要がありますが、「特定の誰かに
依存しない」ことを目的としたソフトのためにそれをするのは筋が通らないと判断しました。
代わりに、全ファイルのチェックサムと、ビルド元のコミットとの対応を検証できる
[ビルド証明](https://docs.github.com/actions/security-guides/using-artifact-attestations)
を公開しています。

実行前に、ダウンロードしたものを照合してください。

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing     # macOS / Linux
```

```powershell
Get-FileHash .\ourvideo-windows-x86_64.zip -Algorithm SHA256    # Windows
```

警告の回避方法:

* **macOS** — バイナリを右クリックして「開く」を一度選びます。または
  `xattr -d com.apple.quarantine ourvideo` で隔離属性を外します
* **Windows** — 「詳細情報」→「実行」
* **Linux** — `chmod +x ourvideo`。警告は出ません

### 自分でビルドする

リリースは[公開されたワークフロー](.github/workflows/release.yml)が
タグの指すコミットからビルドしています。同じことを手元でもできます。

* Rust 1.85 以降
* それだけです。FFmpeg は任意で、あればサムネイルと再生時間が付きます。

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
| `ourvideo doctor` | 環境を点検し、問題があれば対処方法まで表示 |
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
| `ourvideo block cid` / `creator` / `list` | ローカルでの非表示（データも破棄します） |
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

## チャンネル

share-link は**端末**を指します。チャンネルリンクは**人**を指します。

```bash
# 自分の投稿を追いたい人に渡すリンク
ourvideo channel link

# 相手側で一度だけ
ourvideo channel subscribe 'ourvideo://c/…'
```

登録は、端末をフォローするのとは違う 2 つのことをします。**すでに公開済みの
ものを見つけます** — あなたがその投稿者を知る前に流れた Announcement は、
gossip では絶対に届きません。そして**確認できます**。`ourvideo channel refresh`
は「都合よく接続していたか」に頼らず、自分から聞きに行きます。

**相手がオフラインでも機能します。** Announcement には投稿者本人の署名がある
ので、それを保持している別の Node が代わりに渡せます。渡す側は内容を書き換え
られず、でっち上げられず、他人の動画をその投稿者のものとして通すこともでき
ません。だから登録側は**その場にいる誰にでも**聞けます — リンク内のアドレス、
DHT が「答えられる」と言う相手、すでに繋がっている Peer。そして受け取った
answer は毎回自分で検証します。

チャンネルの実体は公開鍵であって、アドレスではありません。投稿者が別のパソコン
に移っても、回線を変えても、Node を丸ごと入れ替えても、登録はその人を指した
ままです。

**誰を登録したかは端末の外に出ません。** 登録を知らせるメッセージは存在せず、
登録者数もなく、投稿者があなたの登録を知る手段もありません。何を見たかと同じ
扱いです。

---

## ルーター（NAT）を越える

多くの人は家庭のルーターの内側からこれを動かします。ルーターは外からの接続を
受け付けないので、そのままでは「取得はできるが配信はできない」ノードになり、
ネットワークはグローバルアドレスを持つ誰かに依存してしまいます。Principle 1 が
防ごうとしているのは、まさにこの状態です。

4つの仕組みが順に働きます。

1. **UPnP** — ルーターにポート転送を依頼します。成功すれば他は不要です。
2. **AutoNAT** — 他のピアに実際に接続してもらい、自分が到達可能かを推測ではなく
   確認します。
3. **Relay（中継）** — 到達不能なら、到達可能なピアに枠を予約し、他人が呼べる
   アドレスを手に入れます。**到達可能なノードは全員が中継役を務めます。**
   指定された中継役はおらず、一覧を公開する場所もありません。中継役も他のものと
   同じ方法で見つけます。
4. **Hole punching（DCUtR）** — 中継経由の接続ができたら、両側が同時に相手へ
   接続を試みます。多くのルーターはこれを通すので、中継役が外れて直接接続になります。

`ourvideo status` と管理画面に、今どの状態かが表示されます。自分でポート開放済みなら
`--external-addr` で最初から答えを教えられます。

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
**ストリーミング再生**。Peer から Chunk を数個先まで先読みするので、全部ダウンロード
し終わる前に再生が始まり、次の Chunk を待って止まることもありません。`Range` に
対応しているのでシークもできます。
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

**キーボードだけでも、スクリーンリーダーでも操作できます。** タブ順の先頭に
「本文へ移動」リンクがあるので、ヘッダーの十数個のコントロールを順に通らずに
本文へ入れます。ページを切り替えるとフォーカスもそこへ移ります。操作できる要素には
すべてフォーカス枠が出ます。進捗・エラー・接続状態はライブリージョンなので、画面の
隅に描かれるだけでなく読み上げられます。配色は明暗どちらのテーマでも WCAG AA を
テストで検証しています — 文字は載る背景ごとに 4.5:1、操作できる要素の枠線は 3:1。
OS で「視覚効果を減らす」を指定している場合はアニメーションを完全に止めます。

ページは厳格な `Content-Security-Policy` の下で配信され、この Node 以外からは
何も読み込みません。また `Host` が loopback 名でないリクエストは拒否するので、
DNS rebinding も塞いでいます。

サムネイルは FFmpeg があれば生成し、通常のコンテンツ Block として保存・P2P 配信します。
FFmpeg が無い場合はサムネイルが付かないだけで、公開は成功します。

---

## 開発

```bash
cargo test --workspace      # 326 テスト（受け入れテスト A〜H を含む）
cargo clippy --workspace --all-targets
cargo fmt --all
```

長時間かかる3つのテストは既定でスキップされます。ノードが動き続けたときにしか
現れない種類の不具合を見つけるためのものです。

```bash
# ワイヤに触れる全パーサへ、壊れたメッセージを数百万件通す
OVN_FUZZ_ITERATIONS=5000000 cargo test -p ovn-node --test untrusted_input --release

# ネットワークを動かし続ける（投稿・取得・視聴、ピアの出入り、キャッシュの破棄）
OVN_SOAK_SECONDS=900 cargo test -p ovn-node --test soak --release -- --ignored --nocapture
```

[CI](.github/workflows/ci.yml) は GitHub のマシン上で同じ3つを実行しますが、
**指示したときだけ**動きます。

```bash
gh workflow run ci.yml                        # Linux のみ
gh workflow run ci.yml -f platforms=all       # Linux / macOS / Windows
```

自動では一切起動しません。非公開リポジトリでは実行時間が課金対象で、倍率も
一定ではないためです（Linux 1倍、Windows 2倍、macOS 10倍）。リリースにタグを
打つ前と、プラットフォーム依存のコードを触ったあとに実行してください。
公開リポジトリにすれば課金がなくなり、この制約自体が不要になります。

`crates/node/tests/acceptance.rs` の受け入れテストは、実際の Node
（実 Identity・実 SQLite・実 libp2p Swarm、ループバックの空きポート）を
プロセス内に複数立ち上げて検証します。テスト専用の環境も専用 DB も作らず、
`ourvideo start` と同じコードパスを通します。

### うまくいかないとき

まずこれを実行してください。

```bash
ourvideo doctor
```

Node が起動していなくても動き、何も書き換えません。問題が見つかったときだけ
終了コードが 0 以外になるので、スクリプトに入れても安全です。点検する内容は、
データディレクトリに書き込めるか、鍵と API トークンが同じマシンの他アカウントから
読めてしまっていないか、データベースが壊れていないか、検索索引が応答するか、
保存済みブロックが今もその ID のハッシュと一致するか、ディスクの残量、FFmpeg の
有無、ポートが空いているか、そして Node が動いていればその Peer 数と外から
到達できるかどうか。見つかった項目には対処方法が併記されます。

```
  ok    database         node.db is intact
  ok    block integrity  128 of 4021 blocks rehashed, all correct
  warn  reachability     behind a router, with no peer relaying yet
                         This node can watch but others cannot reach it to fetch
                         what it publishes. It will keep looking for a relay.
```

`--verify-blocks N` で再ハッシュするブロック数を変えられます（0 ならキャッシュの
点検を省略。キャッシュが満杯のときはこちらが速い）。Node が起動していないときに
どのポートを調べるかは `--port` と `--api-port` で指定します。

個別の症状:


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
* **Hole punching は常に成功するとは限りません。** 拒否するルーターもあり、その場合は
  中継経由のままになります（低速で、中継役が落ちると切れます）。
* **CLI は英語のみです。** Web 画面は多言語化していますが、CLI の出力とヘルプは未対応です。

---

## ガバナンスとライセンス

BDFL 方式です（[GOVERNANCE.md](GOVERNANCE.md)）。コントリビュートは
[CONTRIBUTING.md](CONTRIBUTING.md) と
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) をご覧ください。翻訳が一番はじめやすい
入口です（[docs/TRANSLATING.md](docs/TRANSLATING.md)）。

### 翻訳の協力をお願いしています

英語と日本語は、それを書く人が目を通しています。**Español・Português・العربية は
まだです** — 話せない者が英語から書いたものです。全項目が揃ってはいますが、
揃っていることと自然であることは別です。

この 3 言語のいずれかを読む方へ。`crates/node/src/ui/locales/<code>.json` を開いて
「この文がおかしい」と教えていただくのが、一番ありがたい貢献です。Rust の知識も
ビルドも不要で、1 項目だけの Pull Request でまったく構いません。
[`docs/TRANSLATING.md`](docs/TRANSLATING.md) をご覧ください。

### ライセンス

コードは [GNU Affero General Public License v3 以降](LICENSE) です。

**無料で自由に使えて、改変も自由。許可も支払いも不要です。** 条件は一つだけ —
改変版を配布する場合、またはネットワーク越しに他人が使えるサービスとして
動かす場合は、全ソースを同じライセンスで公開しなければなりません。つまり
**これを取り込んでクローズドにして売ることは誰にもできません**。

**プロトコルはこのライセンスではありません。**
[`protocol/SPECIFICATION.md`](protocol/SPECIFICATION.md) の通信仕様は
[CC BY 4.0](protocol/LICENSE) で、**実装には誰の許可も要りません**
（オープンソースでなくても構いません）。一つの実装しか話せないプロトコルは
その実装の私物であり、それは Principle 1 が防ごうとしているものそのものです。

**名前はコードとは別です。** [`TRADEMARK.md`](TRADEMARK.md) をご覧ください。
フォークも改変も配布も自由ですが、**名前は変えてください** — 改変版を渡された
利用者が区別できるようにするためです。コピーそのものはどのライセンスでも
禁じられません。ここで止めているのは、コピーが本物を騙ることです。
