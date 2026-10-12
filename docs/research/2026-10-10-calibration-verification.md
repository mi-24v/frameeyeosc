# 手動まぶた校正とパネルの実装検証

2026-10-10時点の実装・検証記録。左右別校正、見開き合成と無効化、数値設定、profile保存、測定IPC、パネル操作を実装した。仕様は[まぶた仕様](../design/2026-10-03-eyelid-calibration-design.md)と[IPC仕様](../design/2026-10-06-panel-ipc-design.md)。パネルは独立リポジトリで管理し、この確認の対象は `05c0203`。

## 実装と検証範囲

| 要件 | 実装と確認 |
| --- | --- |
| 左右別の閉眼・自然開眼・任意見開き | 各段階を手動開始し、準備2秒・採取5秒。左右中央値と自然開眼の有効最大値を保存。失敗時は同段階をretry、途中Cancelと開始接続の切断で候補を破棄。 |
| ウィンクを残す補正と上限clip | 左右のOpennessとEyeWideを独立に正規化・filterし、0.75/0.25合成後にcap。未校正時は従来raw経路にcap。見開き無効はEyeWide項だけを止める。実UDPで自然開眼0.75、見開き1、無効時0.75、cap0.4を確認。 |
| 調整できる数値 | 見開き切替、左右margin、最小基礎幅・見開き幅をCLI・保存設定・IPC・パネルから変更。既定値はmargin各0.10、最小幅0.10/0.05。現在profileと成功済み候補を無効にする基礎幅patchは全体拒否。見開き区間不足は測定値を保持し再判定。 |
| サンプル品質 | 80%以上の観測cadenceを基準とし、重複・準備中サンプルを除外。実EyeSource経由で90Hz、producer sequence再起動、mmap交換と復帰、計測境界を検証。新しい外れ値除去や固定標本数は追加していない。 |
| 安全な保存・再測定 | 測定値だけを設定ファイル名+.profile.jsonへatomic保存してから適用。失敗時は旧profile保持。構造・基礎区間の不正は診断してrawで起動し、再校正可能。片眼wideだけが不正なら、その眼の見開きだけ無効化し有効な基礎区間を保持。設定の保存とは分離し、CLI優先順位と再起動後のprofile再利用を確認。 |
| 校正完了時の履歴reset | まぶた履歴だけをresetし視線履歴を保持。実tick経由の非同期wide完了で、One Euroを有効にした実UDP出力へ旧まぶた履歴が混ざらないことを検証。 |
| IPCの因果と状態 | 実snapshot・request ID・caused_byを維持。非同期profile適用の原因はbegin_stage。通常状態も同時に変わる場合、同じ実snapshotで原因付きとnullの通知を分ける。 |
| パネル操作 | 設定と校正の2ページ、実profile・利用不可理由、Start/Begin/Skip/Cancel/retryを表示。Startがbusyで失敗した際のqueued Cancelは他sessionへ送らない。実Rust子とのIPC、fd切断後のconnection_lost照合を確認。keyboard Done・所有子終了の回帰も保持。 |

## 実行したチェック

以下は2026-10-10の実行結果であり、その後の整理ブランチの再検証とは区別する。当時の本体実装・テストは、本ブランチの実装ツリーとテスト用IP置換以外は同一である。

- `cargo fmt --check`、`cargo test --quiet`（126件、2.83秒）、`cargo clippy --all-targets -- -D warnings`、`cargo build --quiet` が成功。
- `python3 -m unittest discover -s scripts -p 'test_*.py'`（49件、0.388秒）が成功。
- パネルDebugのCMake configure/build。`FRAMEEYEOSC_BACKEND`にRustバイナリを指定したCTest全6件が成功（16.67秒、actual-backendは0.44秒）。本体を指定しない独立buildではactual-backendだけskipする。
- 両repoの完全IPC fixtureがbyte一致。契約自体は解析後のJSON等価であり、byte一致はプロトコル要件ではない。
- パネルの実`--help`、両repoの`git diff --check` が成功。

ローカルblack-boxでもABI v4のpacked mmapとrobust mutexを使う90Hz模擬producer、実Unix socket、実UDPで確認した。準備・測定の各段階は7.038/7.029/7.065秒。保存・再起動、取消、候補だけを無効にするpatch拒否、不正profileの診断も確認した。調査用ハーネスとログはリポジトリ外で、永続の回帰検証はRust・パネルtestsに保持する。ハーネスは既存共有メモリを上書きせず、O_EXCLとinode照合で作成・削除を限定し、作成したfixtureのcleanupも確認した。

## 回帰テストで固定した挙動

- Startが拒否された場合、queued Cancelを他sessionへ送らない。誤動作を復元するとfake・実本体双方のテストが失敗した。
- 実EyeSource経由の採取、非同期profile適用のbegin_stage原因保持、実Rustとパネルの照合を確認する。
- 片眼wide不正でも基礎区間を保持し、invalid_wideを返す。
- 非同期wide保存失敗では旧profileとフィルタ履歴を保持する。誤ったresetを入れるとUDP履歴検証が失敗した。
- ディレクトリ書込み拒否の保存失敗テストは、権限を迂回できるUID 0では対象外となる。上記126件は非特権UIDで実行し、保存失敗経路も確認した。

## 検証の限界

ローカル自動検証はx86_64・模擬producerによるものであり、実producerの全環境でのcadenceや分布を保証しない。HMD装着時の分布・快適さ、実アバター、意図しない見開きの完全防止は確認していない。異なるPCへの切替・複数候補・実IPv6 peer/scope送受信は、この実機検証の対象外。自動学習、profile削除、自動再起動は実装範囲外。

製品パネルのSteamVRスタートアップ登録・自動起動、配信切断・停止時の常駐と復帰、Close・ローカルSteamVR終了での両プロセス終了、パネル・本体の異常終了時の動作は実機未確認。実機でのIPC通知到達・再接続・保存後の再起動も未確認であり、ローカル自動テストや調査probeの成功で代替しない。

## 後続の実機パネル更新

本体の校正設定とIPC応答を確認した際、実機パネルは旧版 `e18cf5f` で、helpは `Calibration unavailable in this build.` だった。本体とパネルの更新版を揃える必要がある。

パネル `05c0203` を転送し、Steam Frameのaarch64上でRelease・`CMAKE_INSTALL_PREFIX=$HOME/.local`・`BUILD_TESTING=OFF`としてCMake configure/build/installが成功した。追加のシステム依存は不要だった。旧パネルの測定sessionがnullであることを確認して終了し、インストール済みlauncherから再起動した。本体・パネルが常駐し、state.getはok=true、output=sending、校正設定を返した。HMD上のCalibrationタブとStart/Begin/Skip wide/Cancelの表示については、操作者から確認できたとの報告があった。

この後続確認では更新パネルのARMビルド・インストール・起動・校正UI表示を確認した。各測定段階の完了、実機での保存・再測定、見開き無効化の出力確認は未実施であり、ローカル自動検証と区別する。

## 履歴整理後の回帰確認（2026-10-11）

main起点の整理ブランチで、非特権UIDによるローカル検証を実行した。

- Rust 126件とPython 49件がすべて成功。元のRustテスト126件を同じ名前で保持する。
- `cargo fmt --check`、`cargo clippy --locked --all-targets -- -D warnings`、`cargo build --locked`が成功。テストも`--locked`で依存定義を固定した。
- パネル `05c0203` を別ビルド領域でconfigure/buildし、整理後の本体を指定してCTest全6件が成功。actual-backendは省略していない。
- ローカルblack-boxで準備2秒＋測定5秒、profile保存・再起動、CLI優先、見開き合成・無効化・capの実UDP出力、準備中・測定中・接続切断時のcancel、候補を無効にするpatch拒否、wide省略、不正profileのraw復帰を確認した。模擬producerの共有メモリは検証後に削除した。
- 実装・スクリプト・テスト・依存定義は整理前と照合し、相違は`src/runtime.rs`と`src/target.rs`内のテスト用IP置換だけ。テストが参照する`steamlink-adapter-v4`のUDP観測fixtureもIP・16進表記を置換し、pointerをゼロ埋めした。まぶたJSONL全6ファイルはbyte単位で不変。測定表と両仕様の表・計算例も保持した。
- 本体とパネルのIPC fixtureはbyte一致。ローカル文書リンクを確認し、新しいコミット履歴にも元IP・16進IP・環境固有パスを持ち込んでいない。

この整理で製品ロジックは変更していない。元ブランチとパネルの追跡ファイルも変更していない。今回の確認はローカル回帰検証であり、上記の実機未検証範囲は残る。
