# frameeyeosc 機能追加の調査報告

調査日: 2026-10-03。一次資料と当時のローカルコードの調査結果に、最終仕様の判断を併記する。固定リビジョンの観測と現在の製品実装・検証結果を区別する。
現行契約は[まぶた仕様](../design/2026-10-03-eyelid-calibration-design.md)と[IPC仕様](../design/2026-10-06-panel-ipc-design.md)、製品の確認範囲は[校正検証報告](2026-10-10-calibration-verification.md)に記録する。

## 現在の意図と確定事項

- SteamLink の接続先を使い、付属ワイヤレスアダプタ経由でも OSC 送信先を検出する。
- SteamVRダッシュボードの設定パネルはC++の独立ローカルリポジトリ `frameeyeosc-panel` とする。
- EyeOpenness の上限と左右別キャリブレーションを追加する。左眼が開きやすく見える差を、左右別の補正と最終 clip で補正し、ウィンクを保持する。平均などによる追加均一化は行わない。
- 内部 Openness と EyeWide から最終 EyeLid を合成し、見開き項だけをCLI・設定で無効化できる。
- 自然開眼5秒の追跡有効な有限サンプル最大値へ左右別marginを加えて見開き開始値を決める。`d=(max-relaxed)+margin` とし、既定marginは左右各0.10。CLI・パネルから変更できる。閉眼〜自然開眼の線形区間を維持し、見開き区間不足では余裕を黙って縮めず、その眼のEyeWide=0とする。
- 各段階は手動開始・準備2秒・計測5秒。有効率80%以上は期待producer更新数に対する有効な一意サンプル数で、poll・同一timestampを数えない。無効・短いinactiveは欠測として段階終了時に判定する。約90Hzなら約450期待・約360有効だが、固定360個は要求しない。観測cadenceに基づく率は分布やnominal producer頻度を保証しない。
- IPCは要求ID対応の応答と状態・測定通知を分け、実適用設定と保存結果を返す。各段階の手動開始、見開き省略、途中cancelを提供し、接続復旧後は最新snapshotを取得する。未確認操作は自動再実行しない。
- ABIはv4を基準に検証し、`shm_version`のauto/4/5で選択する。autoは既定で、headerのversionとsizeを検証する。v5の実producer互換は未検証。
- 2026-10-04 JST の実機調査では IPv4 配信、Overlay の非表示/Close、ローカル SteamVR 再起動時の一回の autolaunch を観測した。IPv6 は link-local 候補への scope 付き ping までの限定試行で、IPv6 配信・OSC 送信は未検証。調査登録と probe/observer は解除・停止済みで、この実機検証は終了した。当時は製品実装前の観測だった（[実機報告](2026-10-03-steamframe-hardware.md)）。

調査基点は `mi-24v/frameeyeosc` の `e225dc3`。当時の出力は ABI v4、raw opennessの0..1 clampと既存 OptionalEyeFilterだった。追加機能の現行実装はREADMEと仕様書を参照する。

## 調査対象と固定リビジョン

| 対象 | 確認した SHA | 調査範囲 |
| --- | --- | --- |
| [mi-24v/frameeyeosc](https://github.com/mi-24v/frameeyeosc) | `e225dc3`（ローカル基点） | `src/main.rs` と既存フィルタ、ABI・出力経路 |
| [sasaken1102r/frameeyeosc](https://github.com/sasaken1102r/frameeyeosc/tree/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d) | `af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d` | 接続先検出、まぶた補正、設定・状態、C++ パネル |
| [sboys3/frameeyeosc](https://github.com/sboys3/frameeyeosc/tree/589ff8cecf47545f6e5d1e8128e4aa7fcaf13961) | `589ff8cecf47545f6e5d1e8128e4aa7fcaf13961` | `src/main.rs` の openness 出力経路。raw clamp が中心 |
| [pikepikeid/Baballonia](https://github.com/pikepikeid/Baballonia/tree/63ea0c21effd5bd717d150c59d6fd7d0b548af2f) | `63ea0c21effd5bd717d150c59d6fd7d0b548af2f` | Babble 本体のキャリブレーション適用、VRCFT 出力、ライセンス |

[upstream の forks API](https://api.github.com/repos/konsti219/frameeyeosc/forks?per_page=100) は上記の mi-24v、sasaken1102r、sboys3 の 3 fork を返した。fork の全履歴・全ブランチや実機動作の網羅調査ではない。GitHub main は調査中にも変わり得るため、実装で参照する際は上記 SHA を使う。

追加調査で[sasaken の子 forks API](https://api.github.com/repos/sasaken1102r/frameeyeosc/forks?per_page=100)と各mainの履歴先頭3件を確認した。当時の最新コミットは次のとおり。独自実装の全差分は未確認であり、有用な追加実装がないと判断したものではない。

| 子 fork | 最新コミット（短縮 SHA） | コミット件名 |
| --- | --- | --- |
| [touma-tw/frameeyeosc](https://github.com/touma-tw/frameeyeosc) | `af2610e4` | Merge pull request #12 from sasaken1102r/release-0.7.0 |
| [ronaldburns/frameeyeosc](https://github.com/ronaldburns/frameeyeosc) | `2dadc7ee` | README: ファイアウォールの手順に ModuleProcess とパブリック扱いのネットワークのことを足す |
| [winning-JP/frameeyeosc](https://github.com/winning-JP/frameeyeosc) | `2dadc7ee` | README: ファイアウォールの手順に ModuleProcess とパブリック扱いのネットワークのことを足す |
| [geeksville/frameeyeosc](https://github.com/geeksville/frameeyeosc) | `ecbc60dd` | 0.5.2 公開前のレビューを受けて直す（IP の先頭ゼロを断る・ダッシュボードを閉じたらテンキーを閉じる）と、CHANGELOG に公開日を入れる |

## Babble 本体で実際に補正を適用する箇所

[ParameterSenderService.cs](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Services/ParameterSenderService.cs) の `ProcessEyeExpressionData` は、各パラメータの `GetExpressionSettings` を取得し、OSC メッセージを作る直前に `weight.Remap(Lower, Upper, Min, Max)` を適用する。`ProcessNativeVrcEyeTracking` は左右の lid を別々に変換してから平均し、その平均を 0..1 に clamp して 1 から引き、`EyesClosedAmount` を作る。`ProcessFaceExpressionData` には変換後の `settings.Min..settings.Max` への clamp があるが、眼パラメータの一般送信経路には同じ clamp がない。したがって「Baballonia に clip がない」という全体への一般化はできず、送信経路ごとに区別する必要がある。

[FloatExtensions.cs](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Helpers/FloatExtensions.cs) の変換式は次のとおり。

```text
output = min + (value - inputMin) * (max - min) / (inputMax - inputMin)
```

[CalibrationService.cs](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Services/CalibrationService.cs) と [CalibrationParameter.cs](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Services/Calibration/CalibrationParameter.cs) が入力区間と出力区間を保持し、設定を保存・復元する。この仕組みは左右別の入力補正の参考になる。一方、式には入力区間がゼロの場合や非有限値への対処がなく、そのまま境界処理として採用できない。

[BabbleVRC.cs](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/VRCFaceTracking.Baballonia/BabbleVRC.cs) は Openness と EyeWide の重みを別に設定する。モジュールへの入力とアバターへの最終出力の自然開眼値は異なる。以下の VRCFT core 確認で、モジュールの自然開眼 Openness=1 / EyeWide=0 が最終 `v2/EyeLid*=0.75` に変換されることを確認した。

### Baballonia と VRCFT の見開き対応

Baballonia の固定 SHA `63ea0c21effd5bd717d150c59d6fd7d0b548af2f` では、[BabbleOSC L84-L106](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/VRCFaceTracking.Baballonia/BabbleOSC.cs#L84-L106) が `/LeftEyeWiden` と `/RightEyeWiden` を受信し、[BabbleVRC 左眼 L63-L68](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/VRCFaceTracking.Baballonia/BabbleVRC.cs#L63-L68)・[右眼 L89-L94](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/VRCFaceTracking.Baballonia/BabbleVRC.cs#L89-L94) がそれぞれ EyeWide の重みに代入する。**受信モジュールには見開き対応がある。**

一方、通常の眼推論→OSC 送信経路では [EyeProcessingPipeline L92-L102](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Services/Inference/EyeProcessingPipeline.cs#L92-L102) が左右の視線と lid の 6 値を作り、[ParameterSenderService L30-L44](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Services/ParameterSenderService.cs#L30-L44) の Widen マッピングはコメントアウトされている。**確認した通常 producer 経路では独立した Widen 値の生成・送信をしていない。** 受信側が対応していることと、通常アプリが実際に見開きを届けることは区別する。別の producer が Widen メッセージを送る場合の利用可能性を否定するものではない。

VRCFT core は調査時の master を SHA `6432e6a8d85fa7ec5115fc725c6abcb6dbdd4f35` に固定した。[UnifiedExpressionsParameters.cs L57-L61](https://github.com/benaclejames/VRCFaceTracking/blob/6432e6a8d85fa7ec5115fc725c6abcb6dbdd4f35/VRCFaceTracking.Core/Params/Expressions/UnifiedExpressionsParameters.cs#L57-L61) は、左右それぞれを `final EyeLid = 0.75 * Openness + 0.25 * EyeWide` で合成する。Openness=0 / EyeWide=0 なら閉眼 0、Openness=1 / EyeWide=0 なら自然開眼 0.75、Openness=1 / EyeWide=1 なら見開き 1。この合成式自体に clamp はない。[公式の Eye Expression Parameters](https://docs.vrcft.io/docs/tutorial-avatars/tutorial-avatars-extras/parameters#eye-expression-parameters) も、最終 EyeLid の 0..0.75 を openness、0.75..1 を widen と定義する。

今回推薦している自然開眼 0.75 は **VRChat アバターの最終 `v2/EyeLidLeft/Right` 出力** の値。VRCFT モジュールの Openness へ自然開眼 0.75 をそのまま渡すと、EyeWide=0 の場合、最終値は 0.5625 になってしまう。出力方式を増やす際はこの区別が必要。上記はソース確認であり、実機・導入済み VRCFT の版・アバターでの動作は未検証。

### 関連デバイス別 VRCFT モジュールの clip 確認

pikepikeidの以下の固定リビジョンで眼の更新処理を確認した。LinkFT-PICOとVRCFT-ALVRの該当経路には固定0..1のclipがある。全区間を一定倍率で縮小する処理とは異なる。任意のcapによる上限設定があるとは、この調査からは言えない。

| repo / branch / 固定 SHA | 一次ソースの行リンク | 確認した処理 |
| --- | --- | --- |
| LinkFT-PICO / main / `4cefae22f2b2c5ccc5939690b1e412101c7a1530` | [CalculateEyeOpenness L30-L33](https://github.com/pikepikeid/LinkFT-PICO/blob/4cefae22f2b2c5ccc5939690b1e412101c7a1530/SteamLinkVRCFTModule/SteamLinkVRCFTModule/SteamLinkVRCFTModule.cs#L30-L33)、[左右への適用 L69-L73](https://github.com/pikepikeid/LinkFT-PICO/blob/4cefae22f2b2c5ccc5939690b1e412101c7a1530/SteamLinkVRCFTModule/SteamLinkVRCFTModule/SteamLinkVRCFTModule.cs#L69-L73) | 閉眼重みを squint で増やし、0..1 に clamp してから反転。左右独立 |
| VRCFT-ALVR / main / `b5f26d2c856597e739bb89d90627f67d0c2d998f` | [PICO L91-L99](https://github.com/pikepikeid/VRCFT-ALVR/blob/b5f26d2c856597e739bb89d90627f67d0c2d998f/ALVRModule/PicoFaceTracking.cs#L91-L99)、[FB L88-L96](https://github.com/pikepikeid/VRCFT-ALVR/blob/b5f26d2c856597e739bb89d90627f67d0c2d998f/ALVRModule/FbFaceTracking.cs#L88-L96) | PICO と FB は上記と同じ方式。EyeWide は別の shape 重みとして設定 |
| VRCFTPicoModule / master / `c026aa455da60dd50492b3d36c9d360d0c343b8e` | [Updater L112-L125](https://github.com/pikepikeid/VRCFTPicoModule/blob/c026aa455da60dd50492b3d36c9d360d0c343b8e/VRCFTPicoModule/Utils/Updater.cs#L112-L125)、[ExtendedUpdater L21-L36](https://github.com/pikepikeid/VRCFTPicoModule/blob/c026aa455da60dd50492b3d36c9d360d0c343b8e/VRCFTPicoModule/Utils/ExtendedUpdater.cs#L21-L36) | 左右の openness は `1 - blink` の直接代入で、該当代入に clamp はない。EyeGainX/Y は視線に使う倍率 |

前二者の式を要約すると `openness = 1 - clamp(closed * (1 + squint), 0, 1)`。有限入力では結果は 0..1 に収まる。これは squint に応じて閉眼応答を変えた後の範囲制限であり、今回の候補 `min(corrected_openness, cap)` とも `corrected_openness * cap` とも区別する。3 repo の該当眼開き経路では、左右の値を共通値に置き換える均一化や、任意の openness 上限設定は確認できなかった。VRCFTPicoModule にある口の表情の左右対称化は、眼開きの処理ではない。

この clip 調査は固定リビジョンの C# ソースで眼開きの計算・代入を確認した範囲。過去の全履歴、別ブランチ、core 全体の後段処理や実機動作は網羅していない。core の EyeLid 合成式は前節の追加調査で確認した。入力区間の補正と最終値のclipを分ける方式、見開き合成の切替、設定の動的反映を選定した。現在の仕様はまぶた設計・IPC設計に記録し、2026-10-10に追加余裕各0.10・最小基礎幅0.10・最小見開き幅0.05を変更可能な既定値として採用した。

## 参考コードのライセンス条件

Baballonia の [LICENSE](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/LICENSE) は **Babble Software Distribution License 1.0**。原文の見出しは “Non-Commercial Copyleft Provisions”。記載事項として、4 項はライセンス添付、変更表示、権利・帰属表示の保持を要求する。10(a) は非商用利用を限定し、商用利用を許す再ライセンス・デュアルライセンスを禁止する。10(b) は販売・収益化されるハードウェアへの組込みを制限する。10(c) は派生物を同一ライセンスで公開し、ソースを無償公開する条件を定める。

この報告は条件の記録であり、特定の成果物が派生物に当たるという判断ではない。推薦は、入出力の考え方を研究し、要件から独立した実装とテストを作ること。Baballonia のコードを転載・翻訳移植する場合は、現在の MIT リポジトリへの組込み条件を先に整理する。

## 境界条件と設計上の注意点

### キャリブレーションと上限

[sasaken の main.rs](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/main.rs) は `lid_to_vrcft` で閉眼 0、自然開眼 0.75、見開き 1 に変換する。`sync_lids` は小さな左右差を平均へ寄せ、大きな差では寄せない。`LidCalibration` は減衰ヒストグラムの中央値から左右の自然開眼を学習する。

レビュー上の推論・推薦:

- 左右に raw 値の倍率だけを掛けると、閉眼側の閾値も動く。左右別の閉眼値・自然開眼値を使う区間補正のほうが、閉眼 0 と自然開眼の均一化を同時に検証しやすい。
- 入力補正と最終出力上限を別設定として定義する。最終clipを使い、全区間の比例縮小は行わない。見開き合成を無効にした場合、cap >= 0.75 なら自然開眼 0.75 と中間値を保持し、cap < 0.75 では意図的に clip する設計案。
- 有限値、`closed < relaxed`、見開きを使う場合の `relaxed < wide`、最小区間幅を検証し、無効な結果で既存の有効な補正を上書きしない。出力は必ず有限な範囲内に制限する。
- 左右の最終値を常時平均するとウィンクが消える。今回の選定は独立した左右補正のみであり、平均などによる追加均一化を含めない。左右ウィンクの境界テストで保持を確認する。
- 同 fork の自動学習は 90 Hz 相当のサンプル数で減衰・ウォームアップを定義する。可変レートの環境へ採用するなら経過時間で扱うか、90 Hz 前提を明記する。`reading < 1.0` の学習条件は飽和サンプルを除外するため、飽和状態から自然開眼を学べる保証はない。

### ABI v4 と v5、飽和

同 fork の `ShmLayout` は v4 の sample offset `0x152`、size `0x4f21a` と、v5 の offset `0x157`、size `0x4f21f` を区別する。v5 では sample 前に 5 バイトが追加された。共有メモリの version と size の実測確認なしに v4 の構造体で読むことはできない。

ABIの選択キー `shm_version`（auto/4/5）と入力再open・version/size検証は[IPC設計](../design/2026-10-06-panel-ipc-design.md)に定義する。実際のversion/sizeが選択ABIに一致しない場合は診断して送信を待ち、起動時の不正CLI/設定をエラーとする扱いとは区別する。追跡停止・再起動時の再読込も確認する。

同 fork は SteamOS 0.4.3 で自然開眼の raw openness が 1.0 に飽和する事例と対策を記述する。これは参考実装の観測で、対象実機での再現は未確認。自然開眼と見開きが同値なら見開き区間は推定できない。v5 対応だけで見開き情報が復元するとは扱わない。

### SteamLink 検出・再接続

同 fork の `vrlink_socket_inodes` は `/proc/<pid>/exe` の実行ファイル名で vrlink を見つけ、fd の socket inode と `/proc/net/udp{,6}` を照合する。`connected_udp_peer` は connected 状態かつ loopback・unspecified 以外の最初の peer を選ぶ。`Output::refresh` は auto の接続先を 5 秒ごとに調べ直す。

レビュー上の推論・推薦:

- inode 照合は既定 gateway や固定サブネットより根拠が強いが、vrlink が複数の外部 peer を持つ場合に「最初」が配信元 PC と一致する保証はない。IP ごとに重複を除き、異なる候補が複数なら曖昧状態として送信を待つ。
- 検出は設定変更、peer 消失、PC 切替、vrlink 再起動でやり直す。未検出時に loopback や LAN gateway へ自動的に送らない。手動 target を優先する。
- `/proc` 権限、プロセス終了との競合、別ネットワーク namespace を検出不能の理由として扱う。IPv6 の link-local は IP だけでは interface scope を保持できないため、初期対応範囲を明示する。
- CLI単独起動では接続先autoは明示opt-inとし、既存manualの既定動作を保つ。パネル経由は未指定の既定値だけautoとし、保存manualと明示CLIを尊重する。具体的な引数と起動契約はREADMEに記録する。

### パネルの分離と IPC

[panel/CMakeLists.txt](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/panel/CMakeLists.txt) は親の Cargo.toml から版を読み、`../vendor/frame-updater` の C++ ソースにも依存する。panel ディレクトリだけを独立 repo に移してもビルドは成立しない。

[config.cpp](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/panel/src/config.cpp) は一時ファイルと rename で設定を置換し、未知キーを保持する。[status.rs](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/status.rs) は状態を 100 ms 間隔で atomic replacement する。これは部分書込みを避けるが、複数の設定編集者が同時に read-modify-write する場合の更新消失までは防がない。

Rust本体が補正・検出・適用設定と保存の責任を持ち、独立C++パネルが表示・操作を持つ。Unix pathname socket上のNDJSONを使い、保存writerは本体だけとする。単なるconfig保存を適用成功と表示せず、本体の応答で確認する。具体契約は[IPC仕様](../design/2026-10-06-panel-ipc-design.md)に記録する。

開始接続の切断では測定cancel・旧profile維持、測定中のABI patchはbusyで全体拒否。ABI更新失敗では旧設定・保存値・入力を保持し、旧ABIの検証と送信条件が成立すれば継続、不一致なら待機する。宛先・prefix切替の追加停止通知は行わず、実適用prefix・osc_target・outputを応答と通知で確認する。通常のtracking inactive停止通知は維持する。socket権限0700/0600、4096件のcache上限、保存後apply、caused_by保持はIPC契約に従う。

稼働中の設定変更は次のように反映する。具体的な設定キーと更新・保存境界はIPC仕様に従う。

| 設定項目 | 稼働中の変更時の動作 |
| --- | --- |
| 見開き合成の有効・無効 | 即時反映 |
| 最終まぶた出力の上限cap | 即時反映 |
| 左右別に保存・CLI指定できる追加余裕margin | 即時反映。正規化が変わるまぶたの履歴をreset。d・wide_startは派生値 |
| 最小開閉幅・最小見開き幅（2026-10-10に具体化） | CLI・保存・IPCで変更可能。現在profileを無効にする基礎幅更新は拒否、見開き幅不足は受理しその眼のEyeWide=0。派生値と履歴を更新 |
| OSC prefix | 即時反映 |
| One Euroの有効・無効 | フィルタ履歴をresetして反映 |
| One Euroのmin_cutoff・beta・d_cutoff | 3係数の変更時にフィルタ履歴をresetして反映 |
| 接続先のauto・manual選択 | 接続先を再評価し、UDPを再接続 |
| OSC送信先のホスト/IP・port | 接続先を再評価し、UDPを再接続 |
| 共有メモリABIのauto・4・5選択 | 入力を再openし、version・sizeを再検証 |

起動時は明示CLI > 保存設定 > 既定値とし、起動後のパネルの明示更新はCLI初期値も上書きできる。CLI未指定と明示値を区別し、One Euroも明示falseを指定できるようにする。保存値と現在値を区別し、保存値は次回起動に使うが、その起動で同じCLIを指定すればCLIが再優先される。設定表示は現在の適用値と更新元を示し、起動時の指定元を現在値のロックとして扱わない。不正更新では以前の有効設定を保持する。

OpenVR の dashboard overlay 初期化・イベント・終了処理は Context7 の `/valvesoftware/openvr` を確認した。[Valve の IVROverlay 概要](https://github.com/ValveSoftware/openvr/wiki/IVROverlay_Overview) と [DestroyOverlay](https://github.com/ValveSoftware/openvr/wiki/IVROverlay::DestroyOverlay) が一次資料。`VRApplication_Overlay`、`CreateDashboardOverlay`、表示・非表示イベント、SteamVR 終了イベントの処理が必要。非表示時の描画停止とパネル・OSC本体の起動終了は、後述の[パネルとOSCの起動終了](#パネルとoscの起動終了)に従う。

### パネルとOSCの起動終了

実機probeでは配信停止・物理断の双方でOpenVR process通知とpeer消失を観測し、flagから終了意図を識別できなかった。ローカルruntimeは存続したため、配信停止の意図を分類してプロセスを終了させる方式は採用せず、常駐して入力と宛先の復旧を待つ。

Steam FrameのローカルSteamVRスタートアップ登録でパネルを起動し、配信切断・意図的配信停止でも両プロセスを常駐させる。auto送信先未検出時は待機し、再接続後に送信先・入力・追跡の有効条件が揃えば再開する。非表示では描画を止めて常駐する。明示Close・ローカルSteamVR終了で所有本体とパネルを終了し、Close後は手動起動する。同じ管理socketで本体が稼働中なら新本体・パネル起動を拒否し、既存本体へ接続して管理を引き継いだり停止したりしない。非IPC旧版や別socketの本体は導入時に手動停止する。別watcher・自動再起動監視は対象外。

manifestはパネルを起動し、OSC本体は所有する子プロセスとする。パネル異常終了時は所有本体も終了する。本体異常終了時はパネルを残して停止理由を表示し、自動再起動せずClose後に手動起動する。親終了検知、終了待ちと強制終了は[パネルREADME](https://github.com/mi-24v/frameeyeosc-panel#launch-and-lifecycle)に記録する。製品の自動検証と調査probeの観測を区別する。

参考forkの [vr_overlay.cpp L195–201](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/panel/src/vr_overlay.cpp#L195-L201) はSteamVRアイコンバーのCloseを有効にし、[L291–298](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/panel/src/vr_overlay.cpp#L291-L298) はOverlayClosedと非表示イベントを別に処理する。明示終了を区別する参考であり、本実装でのイベント到達は実機で確認する。

自動アップデータ、LiveLink/ETVRの追加出力、視線の再中心化、録画UIは対象外。

## 実装の検証条件

| 領域 | 合格条件 |
| --- | --- |
| 数値補正 | 左右別に閉眼が 0、自然開眼が定義値になり、見開きは対応時のみ上限に到達。閾値の直前・一致・直後で連続・単調・有限。NaN/Inf、等しい・逆転・極小の区間を拒否 |
| ウィンク | 左閉眼・右自然開眼とその逆を保持。左右の自然開眼 raw 値が異なっても補正後は揃う。上限変更後も完全閉眼を保持 |
| 追跡の継続 | 追跡停止・再開、時刻後退、サンプル不足、片眼異常で不正な補正や以前のフィルタ履歴を混ぜない |
| ABI | v4/v5 の version・サイズ・offset fixture が一致。選択と実測の不一致・未知 version・短いファイルで送信しない |
| 検出 | 所有外 inode、非 connected、loopback、unspecified、壊れた行を除外。未検出→接続→切断→同じ IPv4 PC への再接続を確認。異なる PC への切替、複数候補、実 IPv6 peer と interface scope を使った送受信は今回の必須検証から外す |
| 設定・IPC | atomic 更新、中途半端・不正設定で以前の有効設定を維持。未知キーを保持。起動時CLI優先と起動後パネルによる上書き、次回起動時のCLI再優先を検証し、現在の適用値・更新元を表示。補正要求の再読込で重複実行せず、古い結果を今回の成功と表示しない |
| パネル | Steam FrameのローカルSteamVR起動・再起動時のスタートアップ登録によるパネルとOSC本体の起動、非表示時の描画停止と常駐、配信切断・意図的配信停止時の両プロセス常駐、auto送信先未検出時のOSC待機と再接続後の有効条件成立時の送信再開を検証。明示Close・ローカルSteamVR終了で両方終了し、Close後の同セッション内の再利用は手動起動する。同じ管理socketの本体が存在する場合の新本体・パネル起動拒否と、既存本体の管理引継ぎ・停止を行わず二重OSCを防ぐことを検証。manifestはパネルを起動し、OSC本体は所有する子プロセスとする。パネル異常終了時の所有本体終了、本体異常終了時のパネル継続・停止理由表示、自動再起動せずClose後の手動起動を検証。別watcher・自動再起動監視は初期対象外。製品の起動終了・待機復帰・異常終了時の動作とスタートアップ登録による自動起動は実機未検証。自動検証と実機確認は校正検証報告に区別して記録する |

## 現行仕様と検証の境界

追加余裕各0.10・最小基礎幅0.10・最小見開き幅0.05は変更可能な初期値。準備2秒・計測5秒・中央値・有効率80%以上、自然開眼最大値＋margin、未校正raw互換＋capを採用し、新しい外れ値除去は追加しない。見開き未測定・不正・区間不足はその眼のEyeWide=0とし、有効な基礎区間を保持する。profileは保存・再利用でき、有効な再測定で上書きする。削除/reset、自動学習、自動装着検知は対象外。

片眼raw欠落・非有限値・無効ではsnapshot全体を無効にし、両眼の更新を止める。正常raw=0は有効で、片眼だけの更新継続や短時間holdは追加しない。

IPv6、異なるPCへの切替、複数候補の実機送受信は未検証。既存IPv6対応や曖昧候補の待機処理は維持する。初期のprobe調査の詳細・原値を保持したまぶた統計は[実機調査報告](2026-10-03-steamframe-hardware.md)、再装着・初期値比較は[まぶた実測](2026-10-10-eyelid-measurement.md)に記録する。製品の自動検証、ARMパネルのビルド・起動・校正UI表示と残る未検証事項は[校正検証報告](2026-10-10-calibration-verification.md)に記録する。
