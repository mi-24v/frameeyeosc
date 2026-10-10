# Steam Frame 実機調査報告

初期調査日: 2026-10-03。まぶた測定追記: 2026-10-04（JST）。SSH 初期調査は読み取り専用。まぶた測定は共有メモリの通常 mutex 操作と request=1 の書込みを伴う観測で、サービス設定は変更していない。製品機能や接続先自動検出の合格報告ではない。
[機能調査報告](2026-10-03-frameeyeosc-features.md) と [まぶた設計案](../design/2026-10-03-eyelid-calibration-design.md) の実機未確認事項を補足する。

本報告は当時の実機観測を記録する。ネットワークアドレスは匿名化し、IPv4は文書用アドレス帯、IPv6はlink-localの性質を保った例示値へ置換した。16進表記も対応させ、pointer値はゼロ埋めした。ABI・眼開き・PID/inode等の観測値と測定条件は保持する。製品の後続確認は[校正検証報告](2026-10-10-calibration-verification.md)を参照する。

## 接続と実行環境

- SSH による実機への接続を確認した。接続先アドレスとアカウント名は記録しない。
- CPU アーキテクチャは `aarch64`。
- `/etc/os-release`: SteamOS、`VERSION_ID=0.3.0`、`BUILD_ID=20260922.6101926`、`VARIANT_ID=vr`。
- `python3`、`cmake`、`ninja`、`g++`、`pkg-config` は `/usr/bin` に存在する。実機の `cargo` は PATH 上で見つからなかった。
- pkg-config の版は cairo `1.18.0`、freetype2 `26.1.20`、vulkan `1.4.309`。`/opt/steamvr/bin/linuxarm64/libopenvr_api.so` は存在し、245064 バイト。パネルのビルド・描画は未検証。
- SSH シェル、`vrlink`、`eyetracking`、`steam`、`vrserver` の network namespace はいずれも `net:[4026531833]`。今回の接続で一致した観測であり、別 namespace が一般に不要という判断ではない。

## 共有メモリとプロセス

- `/dev/shm/eye-server.mmap` は 324122 バイト（`0x4f21a`）。先頭の u32 は `[4, 1]` で、ABI v4・初期化済みを示す観測と整合する。配信中の再確認でも同じヘッダとサイズだった。
- `eyetracking` PID 2547、`steam` PID 2554、`vrserver` PID 2277、`vrcompositor` PID 2373、`vrdashboard` PID 2463 を確認した。
- 最初の調査では `vrlink` が不在だった。付属アダプタ経由の配信中は `vrlinkrunthread` PID 9272 を確認し、exe は `/opt/steamvr/tools/vrlink/bin/linuxarm64/vrlink` だった。表示名だけでなく exe を確認した。
- raw eye openness は下記の閉眼・自然開眼・見開き・左右ウィンク測定で採取した。製品機能によるウィンク保持は未検証。

## 初期ネットワークの観測

| interface | 状態 | IPv4 | 確認した IPv6 |
| --- | --- | --- | --- |
| `wlan0` | UP | `198.51.100.16/24` | 今回の出力には表示なし |
| `wlanap` | UP | `192.0.2.1/24` | `fe80::a:1/64` |

- IPv4 routes: default は `198.51.100.1 dev wlan0`、`192.0.2.0/24 dev wlanap`、`198.51.100.0/24 dev wlan0`。
- IPv6 route: `fe80::/64 dev wlanap`。今回の配信では IPv4 peer を確認した。配信での IPv6 link-local peer の使用と送信時の interface scope の必要性は未確認。
- `/proc/net/udp` と `/proc/net/udp6` は読み取れた。最初の調査の connected UDP は家庭 LAN の DHCP `198.51.100.16:68 → 198.51.100.1:67` のみ。Steam の 27036 番ポートは IPv6 wildcard の unconnected socket だった。

## 付属アダプタ経由の配信中に確認した peer

`vrlink` の fd 80 は `socket:[159857]` を指し、`/proc/9272/net/udp` の connected 行に一致した。

```text
 5596: 010200C0:28A0 140200C0:28A0 01 00000000:00000000 00:00000000 00000000 1000 0 159857 2 0000000000000000 0
```

- `ss -uapn` は `ESTAB 192.0.2.1%wlanap:10400 → 192.0.2.20:10400`、`vrlinkrunthread` PID 9272・fd 80 を示した。inode と UDP 行、所有プロセスが一致し、今回の配信 peer の IP `192.0.2.20` を識別する根拠になった。
- 10400 は配信のポートであり、OSC 送信先ポートではない。配信 peer は default gateway `198.51.100.1` と異なる。
- fd 74・inode 159856 は `0.0.0.0%wlan0:10400` の unconnected socket（UDP state `07`、peer `0.0.0.0:0`）であり、接続先候補から除外する。
- 読み取れた udp6 に `vrlink` 所有 inode の connected 行はなかった。この一回の IPv4 観測で IPv6 や scope の汎用対応を不要とは断定できない。
- `ip neigh dev wlanap` で PC の `192.0.2.20` と `fe80::20` が同一 MAC に対応した。PC に link-local IPv6 が存在する観測と、使用中の配信 UDP が IPv4 である観測は区別する。

## 配信停止・再開時の変化

操作者が停止状態を確認した後、`vrlink` がプロセス一覧から消え、`/proc/9272` もなくなった。`/proc/net/udp` から inode 159857・159856 と remote `140200C0` の行が消失し、`ss` にも `vrlink` と `192.0.2.20` は表示されなかった。共有メモリは ABI 4・初期化値 1・324122 バイトを維持した。
再開後は同じ exe の `vrlink` PID 9683、namespace `net:[4026531833]` を確認した。fd 78 が新しい inode 160449 を所有し、`/proc/9683/net/udp` と `ss` に同じ IPv4 peer `192.0.2.20:10400` が再出現した。fd 76 の `wlan0:10400` は unconnected として除外する。udp6 の所有 connected 行はなかった。
配信開始前の不在 → 開始 PID 9272・inode 159857 → 停止後の消失 → 再開 PID 9683・inode 160449・同じ peer を観測した。自動検出コードによる状態遷移の検証ではない。

[観測の抜粋データ](fixtures/steamlink-adapter-v4/README.md) に開始前・配信中・停止・再開の UDP 対象行と socket 所有情報を保存した。全 raw 出力の保存ではなく、自動テスト用素材。列ヘッダの正規化と保持した一次データの範囲を同 README に明記した。

## 配信終了とローカル SteamVR の継続

別の観測で、配信中の 2026-10-03 21:20:55 UTC（2026-10-04 06:20:55 JST）、停止後の 22:29:12 UTC（07:29:12 JST）、再開後の 22:31:08 UTC（07:31:08 JST）、VRChat 終了申告後の 22:32:19 UTC（07:32:19 JST）のプロセス・UDP snapshot を比較した。原ログはリポジトリ外の `frameeyeosc-lifecycle-streaming-{before,stopped,restarted}.txt` と `frameeyeosc-lifecycle-vrchat-exited.txt` に保存した。操作者は最初に VRChat の配信だけを停止・再開し、その後 VRChat 自体を終了したと申告した。ホスト PC のゲームプロセスは直接観測していない。以下は必要な値の要約で、引数やログの内容は転載しない。

| 対象 | 配信中 | 停止後 | 再開後 | VRChat 終了申告後 |
| --- | --- | --- | --- | --- |
| `vrserver` / `vrcompositor` / `vrdashboard` | PID 2273 / 2350 / 2442 | 同じ PID が残存 | 同じ PID が残存 | 同じ PID が残存 |
| `vrlink` / `run_vrlink.sh` | PID 3943 / 3940 | 一覧から消失 | 新 PID 81300 / 81297 | 一覧から消失 |
| `frameeyeosc` | PID 5093 | 同じ PID が残存 | 同じ PID が残存 | 同じ PID が残存 |
| 配信 UDP | `192.0.2.1:10400 → 192.0.2.20:10400`、inode 109503 | 10400 socket が消失 | 同じ peer、新 inode 525547 | 10400 socket が消失 |
| OSC 9000 宛 UDP | inode 23921 | 同じ inode が残存 | 同じ inode が残存 | 同じ inode が残存 |

この実機の今回の観測では、配信終了はローカル SteamVR プロセスの終了と一致しなかった。OSC socket の残存は送信や PC 側の受信を確認したことを意味しない。途中で SSH master が消失し再認証したため、停止瞬間のイベントは観測できず、その原因も不明。
再開時には配信プロセスと socket が新たに現れ、VRChat 終了申告後には再び消失した。今回の起動・終了操作では、ゲーム終了後も配信が継続する予想と異なる観測になった。ただしゲーム終了一般の挙動へは一般化せず、ゲーム終了の意図を配信プロセスや socket の有無だけで判別できるとは扱わない。
この初回 snapshot 比較では専用パネルと Overlay イベント probe がなく、Overlay イベントやパネル終了は未観測。プロセスが残る事実だけではパネルの寿命・表示動作は判断できない。後続の物理断試験は下記に区別して記録する。

## 物理アダプタ断と元ゲームへの再接続

当時の検証記録では、調査用 `probe.cpp` と `observer.py` の実機 g++ Werror ビルド、Python compile/help/smoke、OpenVR 初期化と timeout 終了時の cleanup が成功したと報告されている。実行 runtime は `2.17.10`。調査probeは専用パネルや製品機能の実装ではない。
observer PID 84445 と probe PID 84848 を実機で nohup により起動した。SSH 起動コマンド終了後も PPID=1 で継続し、実機の一時領域のログ更新を確認した。この旧試験までの時点では probe 開始後の SSH 通信断は実測していなかった。observer は PID と `/proc` の starttime、UDP peer/inode、probe は system/overlay/thumbnail queue のイベントと heartbeat を記録した。証拠はリポジトリ外の調査ログの `observer.jsonl`、`events.jsonl`、`before-unplug.txt`、`after-unplug.txt`、`reconnected-before-explicit-stop.txt`、`after-explicit-stop.txt`。raw 全ログはリポジトリへ保存していない。

- 操作者はアダプタを約 5 秒抜いて戻したが配信は終了し、PC 側 SteamVR/VRChat は続いているようだと申告した。ホスト PC のプロセスは直接観測していない。
- UTC 2026-10-03 22:55:34.395（JST 2026-10-04 07:55:34.395）の observer 記録で `vrlink` PID 85053 と UDP inode 557432 が消失。peer は `192.0.2.20:10400`、local は `192.0.2.1:10400`。これは polling で変化を検出した時刻であり、物理断の正確な瞬間ではない。
- ローカルの `vrserver` 2273、`vrcompositor` 2350、`vrdashboard` 2442、`frameeyeosc` 5093、probe 84848 は PID/starttime を維持した。probe は後続 heartbeat でも `vrserver=alive`、Overlay 問い合わせ成功を記録した。
- UTC 22:55:34.302 に `ProcessQuit=701`・`ProcessDisconnected=1307`・`SceneAppPipeDisconnected=413`、22:55:34.342 に `DashboardActivated=502` を各 queue で観測した。`Quit=700` は確認できなかった。event union の対象 PID は記録しておらず、誰の終了通知かは特定できない。
- UTC 22:55:35.147 に `OverlayDestroyed=540` も記録されたが対象 Overlay は未特定。未知イベント 114 なども多く、既知の Quit がないことを全通知の不存在へ一般化しない。probe 自身の終了は観測されず、専用パネルの寿命は未検証。
- 操作者は元の VRChat への再接続成功を申告した。再接続後 snapshot（採取 UTC 22:59:15、JST 07:59:15）で新 `vrlink` PID 85776・starttime 3580150、新 UDP inode 564349 と同じ peer を確認した。ローカル runtime・probe・`frameeyeosc` は同じ PID/starttime を維持した。

再接続後、操作者が意図的な配信停止を実行したと回答した。同じ logger の UTC 22:59:39.535（JST 07:59:39.535）で `vrlink` PID 85776・inode 564349 が消失し、ローカル runtime・probe・`frameeyeosc` は同じ PID/starttime を維持した。UTC 22:59:39.398 に `ProcessQuit=701`・`ProcessDisconnected=1307`・`SceneAppPipeDisconnected=413` を各 queue で観測し、`Quit=700` は確認できなかった。`DashboardActivated=502` はその約 5 秒前の 22:59:34.361 で、停止前の UI 操作を含む可能性がある。
この旧 probe 試験では物理断と意図的停止の両方で配信 PID/socket の消失と上記イベントを観測したが、event union の `pid`・`bForced`・`bConnectionLost` は未記録だった。両操作の意図を確実に区別する根拠は得られず、field を追加して下記の再調査へ進んだ。自動復帰、OSC の送受信、製品パネルの停止・再開動作の合格も未確認。

## 調査 Overlay の非表示・Close と ARM manifest 登録

証拠はリポジトリ外の調査ログの更新イベント記録と `after-dashboard-hidden.txt`、`after-overlay-close.txt`、`manifest-arm-evidence.txt`、`registered-launch-status.txt`。以下の時刻は UTC 2026-10-03 で、9 時間を加えると JST 2026-10-04 となる（例: 23:02:26.037 UTC = 08:02:26.037 JST）。コード・生ログは一時領域の調査用で、製品パネルの実装ではない。

- 操作者が縞模様の probe Overlay を表示して dashboard を閉じたと申告した操作では、23:02:26.037 に `OverlayShown=500`、23:02:27.649 に `DashboardDeactivated=503`、23:02:27.669 に `OverlayHidden=501` を観測した。PID 84848 は存続し、後続 heartbeat は dashboard/Overlay が非表示・非 active、問い合わせ成功、`vrserver=alive` を記録した。
- probe 自身の Close 操作では、23:03:27.966 に Overlay queue の `OverlayClosed=534`、`exit_requested`（`overlay_closed`）、Overlay destroy 成功を記録し、23:03:27.968 に shutdown 完了・exit code 0 を確認した。observer でも probe の消失を確認し、ローカル runtime と `frameeyeosc` は存続した。非表示と、この probe が実装した Close による終了を区別する。
- 初回の manifest は `binary_path_linux` だけでは Add が成功を返しても `installed=false` となり、rollback した。vrserver ログに binary path 必須による skip を確認し、実機の builtin `runtime.vrmanifest` が `binary_path_linux_arm` を持つことを一次資料で確認した。この key を調査 manifest に追加した後、登録成功を確認した。
- 固定 app key `local.frameeyeosc.lifecycle-probe.launch`、調査用の固定 launcher `launcher.sh` の status は `installed=true`・`own_registration=true`・`dashboard_overlay=true`・`autolaunch=true`。helper の明示 launch により `LaunchDashboardOverlay` を呼び、PID 87094、OpenVR 初期化・dashboard Overlay 作成成功、runtime `2.17.10` を確認した。自動起動で開始した観測ではない。

この登録試験時点では調査登録が有効で、検証後に登録と autolaunch を解除する予定。この段階では配信開始に伴う登録アプリの自動起動、SteamVR 再起動、登録・起動の反復は未検証だった。後続の比較結果は以下に記録する。

## ProcessQuit の対象 PID と flag を追加した比較

新 probe の証拠はリポジトリ外の調査ログの `autolaunch.jsonl`、更新 `observer.jsonl`、`after-unplug-v2.txt`、`after-unplug-v2-manual-restart-stop.txt`。以下は UTC 2026-10-03 の時刻で、9 時間を加えると JST 2026-10-04 となる（例: 23:14:17.359 UTC = 08:14:17.359 JST）。操作者は物理抜差し後に自動復旧せず、一度手動で配信を再開してから停止したと申告した。操作の正確な瞬間は記録しておらず、以下はログの観測時刻。

- 物理断側では UTC 23:14:17.359 の `ProcessQuit=701` が対象 PID 88077、`old_process_pid=0`、`forced=false`、`connection_lost=false` を記録した。23:14:17.392 の observer は同 PID の fd 一覧取得で `PermissionDenied` となり、その snapshot の socket 状態は確認できない。23:14:17.597 に PID の消失とエラー解消を記録した。終了との競合の可能性はあるが、権限エラーの原因は断定しない。
- 手動再開は UTC 23:14:36.696 に新 `vrlink` PID 88654・inode 593142 と同じ peer `192.0.2.20:10400` が出現。手動停止は 23:14:47.341 に同 PID/inode が消失し、23:14:47.342 の `ProcessQuit` が PID 88654、old PID 0、forced=false、connection_lost=false を記録した。
- ローカル runtime 2273/2350/2442、probe 87094、`frameeyeosc` 5093 は同じ PID/starttime を維持した。helper Utility の終了通知 PID 87129/88267/88904 は配信プロセスの終了通知と区別する。
- 今回は SSH 共有接続が Broken pipe/reset で切れ、再認証後も boot ID は不変だった。observer の UTC 23:07–23:11 の範囲で heartbeat 24 件が継続し、SSH 切断区間にも実機ログが更新されたことを確認した。

`connection_lost` は OpenVR の process イベントに含まれる flag であり、SteamLink 経路の障害や操作者の終了意図を直接示すと仮定しない。今回の物理断と手動停止では同じ false で、forced も false のため分類根拠にはできない。この比較 snapshot 時点では調査登録・autolaunch=true と probe 87094 が継続していた。後続の Close・配信再開試験は次節に記録する。調査全体の完了や製品機能の合格とは扱わない。

## 登録 probe の Close 後に配信を再開した結果

証拠はリポジトリ外の調査ログ `registered-overlay-closed.txt` と `stream-restart-autolaunch-check.txt`。UTC 2026-10-03 23:16:32.573（JST 2026-10-04 08:16:32.573）に probe 87094 の `OverlayClosed=534`、exit request、Overlay destroy 成功を記録し、23:16:32.574 に exit code 0 で終了した。23:16:50 の status は installed=true・autolaunch=true・pid=0。登録は残り、probe は停止した。
操作者が配信を再開した後、UTC 23:19:00.321（JST 08:19:00.321）に新 `vrlink` PID 89377・starttime 3704720、新 inode 598782 と同じ peer `192.0.2.20:10400` の出現を確認した。約 26 秒後の 23:19:26 の status は installed=true・autolaunch=true・pid=0 で、observer に probe はなく、ローカル runtime の PID/starttime は不変だった。Close 以降に明示 launch を呼んでいない。
今回の試行では配信再開だけで登録 probe の自動起動は確認できなかった。autolaunch 設定の有効化だけで配信ごとの起動を保証するとは扱わない。この配信再開 snapshot 時点では登録は有効・probe は停止中だった。後続のローカル SteamVR service 再起動試験は次節に記録する。

## Steam Frame ローカル SteamVR の再起動と自動起動

証拠はリポジトリ外の調査ログの `before-steamvr-restart.txt`、`after-steamvr-restart.txt`、更新 `autolaunch.jsonl`・`observer.jsonl`。Steam Frame の user unit `steamvr.service` の ExecStop（vrstartup の shutdown）を確認し、`systemctl --user restart --no-block` を実行した。ホスト PC の SteamVR を再起動した操作ではない。
Quit 採取のため再起動前に probe を明示 launch し、PID 89657、旧 runtime 2273/2350/2442 を確認した。UTC 2026-10-03 23:20:27.656–.657（JST 2026-10-04 08:20:27.656–.657）に各 queue で `Quit=700`、対象 PID 89657・forced=true・connection_lost=false を記録した。probe は Quit を acknowledge し、終了理由 `vr_quit` と 23:20:27.658 の shutdown 完了・exit code 0 を記録した。
UTC 23:20:32.935（JST 08:20:32.935）に新 probe PID 90265 が開始し、OpenVR 初期化と Overlay 作成成功、新 runtime `vrserver` PID 90069 を記録した。service 再起動後に明示 launch を呼んでいない。後続 snapshot は service MainPID 90240・active/running、runtime PID 90069/90128/90230、installed=true・autolaunch=true・probe PID 90265 を示した。boot ID は不変で、`frameeyeosc` PID 5093・starttime 76953 は継続した。
一回のローカル SteamVR 起動時 autolaunch 成功を確認した。上記の配信再接続だけでは約 26 秒後も未起動だった結果と区別し、反復や製品パネルの合格へ一般化しない。PC側は存続しているとの操作者の申告があったが、ホスト PC の SteamVR/VRChat のプロセスは直接観測していない。
この再起動後 snapshot では調査登録・autolaunch 有効、probe 90265 が継続中だった。後続の IPv6 限定確認と調査登録・プロセスの解除結果は次節に記録する。

## 最終 IPv6 限定確認と調査用登録の解除

証拠はリポジトリ外の調査ログの `ipv6-preflight.txt`、`ipv6-streaming.txt`、`ipv6-candidate-scoped-ping.txt`、`ipv6-candidate-scoped-ping.err`、`registration-removed.txt`、最新の observer/probe ログ。UTC 2026-10-03 23:23:48（JST 2026-10-04 08:23:48）の配信中 snapshot は `vrlink` PID 91380、`192.0.2.1%wlanap:10400 → 192.0.2.20:10400`、inode 623713 の IPv4 UDP を示し、IPv6 UDP は確認できなかった。
この時点の `wlanap` は ifindex 14、link-local `fe80::a:2/64`。候補 `fe80::20` は初回には IPv4 PC と同じ MAC に対応したが、最新 neighbor 状態は FAILED で、この再試験では対応を再確認できなかった。候補に `%wlanap` を付けた scoped ping は exit code 1、2 送信・0 受信、stderr は空だった。interface scope を明示した限定試行であり、実 IPv6 peer による配信・OSC 送受信・到達性の合格は未検証。候補の不応答だけで IPv6 全般の未対応を結論しない。
helper の remove を実行し、remove/status の installed=false を確認した。probe 90265 は UTC 23:27:41.048（JST 08:27:41.048）に SIGTERM を受け、Overlay destroy 成功、23:27:41.050 に exit code 0 で終了。observer 84445 は 23:27:41.069 に SIGTERM で停止した。両 PID の不存在を確認し、既存 `frameeyeosc` PID 5093 と配信 PID 91380 は継続した。
一時的な調査登録を解除し、この実機検証を終了した。調査コード・生ログは一時領域のみで、製品機能は未実装。IPv6 実配信・送信、製品の接続先再検出・まぶた補正・IPC・パネルの合格を今回の限定試験から主張しない。

## 閉眼・自然開眼・見開き・左右ウィンクの raw 測定

この採取はRust製品の校正・合成の実装前に行った。各状態は settle 2 秒後に約 5 秒測定し、ABI 4・初期化値 1・324122 バイトを確認した。[metadata/summary の抜粋](fixtures/eyelid-v4/README.md) を保存し、raw 全ログはリポジトリへ保存していない。
metadata の remote `wall_time` を変換すると、自然開眼は 2026-10-04 02:54:00.580、閉眼は 02:55:48.526、見開きは 02:57:08.876 JST（UTC はそれぞれ 2026-10-03 17:54:00.580、17:55:48.526、17:57:08.876）。日時は metadata の記録値で、時計の同期精度は未検証。

| 状態 | valid | 左 median | 右 median | 左 p05–p95 | 右 p05–p95 |
| --- | --- | --- | --- | --- | --- |
| 閉眼 | 450 | 0.027368 | 0.085941 | 0.002494–0.061175 | 0.055006–0.120629 |
| 自然開眼 | 449 | 0.781757 | 0.693278 | 0.713407–0.813768 | 0.648904–0.721926 |
| 見開き | 449 | 0.995161 | 0.824072 | 0.931879–1.000000 | 0.800258–0.847856 |
| 左ウィンク | 448 | 0.031010 | 0.430450 | 0.000000–0.091671 | 0.269642–0.499362 |
| 右ウィンク | 449 | 0.515628 | 0.120177 | 0.449381–0.832747 | 0.000000–0.171617 |
| 右ウィンク再測定 | 446 | 0.570096 | 0.040169 | 0.529547–0.895603 | 0.000000–0.087414 |

inactive、invalid openness、nonfinite/duplicate/backward timestamps、busy timeout は全状態で 0。自然開眼・見開きでは mutex busy の retry が各 1 回あり、timeout や欠測として扱われたものはなかった。
この装着・一回の測定では左右とも自然開眼 p95 < 見開き p05 で、raw 分布を区別できた。見開き左は最大 1.0、raw >= 0.999 の割合 0.429844 で上限付近に集まったが、自然開眼との分離は残った。左右別補正の参考値であり、測定閾値・デッドゾーン・既定 profile の確定根拠とは扱わない。
自然開眼の p95 − median は左 0.032011、右 0.028648 で、比較用のデッドゾーン候補 d=0.02（現行既定値ではない）を超えた。この median を relaxed に使う場合、自然開眼の揺れでも EyeWide が正になる可能性がある。漏れの実際の見え方やフィルタの効果は未検証で、d の値や式を今回の測定だけで確定・変更しない。
左ウィンクは 2026-10-04 02:59:50.091 JST（UTC 2026-10-03 17:59:50.091）、右ウィンクは 03:01:39.817 JST（UTC 18:01:39.817）の metadata で採取した。両方向とも閉じた側 median < 反対側 median だった。反対眼 median は左ウィンクの右 0.430450、右ウィンクの左 0.515628 で、それぞれ自然開眼の 0.693278・0.781757 より低かった。
ウィンク中の反対眼の完全開眼維持は難しいとの操作者の申告があった。実際の反対眼開度の独立した基準測定はなく、raw の低下をセンサ干渉や不具合とは断定できない。力みや装着条件の影響も未確認。左右独立正規化・平均しない設計を維持し、反対眼の強制全開や完全開眼保持という受入条件は追加しない。
右ウィンクを開始合図の直後に再測定した。metadata 時刻は 2026-10-04 03:06:10.997 JST（UTC 2026-10-03 18:06:10.997）。valid 446、異常カウンタと busy retry は 0。右 median は前回 0.120177 から再測定 0.040169、左は 0.515628 から 0.570096 だった。旧測定を保持して別記録とし、この差だけで前回値の誤りやセンサ不具合とは判断しない。

## 未検証範囲と次の採取

- 異なる PC への切替、複数候補、実 IPv6 peer と interface scope を使った送受信は今回の検証対象外とする。これらは未検証のままで、既存の IPv6 対応を削除する指定ではない。同じ IPv4 PC への停止・再開に伴う PID/inode の変化と peer の再出現は観測済み。
- 保存した抜粋データを基に parser・再検出を実装検証し、必要な追加データを採取する。自動検出の検証済み実装とは扱わない。
- 装着変更と再測定、左右ウィンクの出力保持、提案閾値、製品実装の補正・合成・clip と保存後の再利用を検証する。今回の raw 分布の分離は製品機能の実機合格を意味しない。

最初の `vrlink` 不在は配信中の追加観測で解消した。送信先自動検出の実装・まぶた合成の実機合格は未確認。
ローカル自動テストの成功は、実機での新機能検証を代替しない。後続の製品検証は[校正検証報告](2026-10-10-calibration-verification.md)に区別して記録する。
