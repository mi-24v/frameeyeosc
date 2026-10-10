# 付属アダプタ配信の UDP 観測抜粋

採取日: 2026-10-03。SSH による読み取り専用の実機調査から、テスト用素材として保存した。

| ファイル | 保存範囲 |
| --- | --- |
| `udp-before.txt` | 開始前は vrlink 不在。対象所有行なし |
| `udp-streaming.txt` | PID 9272・fd 80・inode 159857 の connected IPv4 UDP 行 |
| `udp-stopped.txt` | 停止後は vrlink 不在。旧 inode 159857/159856・配信 peer の行なし |
| `udp-restarted.txt` | PID 9683・fd 78・inode 160449 の connected IPv4 UDP 行 |
| `socket-ownership.txt` | exe、namespace、fd/inode、peer と観測条件の対応 |

UDP ファイルは `/proc/net/udp` または `/proc/<pid>/net/udp` の全出力ではなく、対象の vrlink 所有 connected 行だけを抜粋した。開始前・停止後のヘッダだけのファイルはその対象行がないことを表し、機器に UDP socket が一切ないことを表さない。DHCP、Steam の socket、unconnected socket の完全な行は保存していない。

列ヘッダは proc の列順を表すため追加・正規化したもので、採取時のヘッダそのものではない。IPv4 は文書用アドレス帯へ置換し、UDP 行の16進表記も一致させた。IPv6 は link-local の性質を保持して識別部分を置換し、pointer 列はゼロ埋めした。元アドレスとの対応表は保存していない。PID・fd・inode・ポート・状態・queue 値・空白は観測値のまま保持し、所有関係と停止・再開時の変化を検証できるようにした。

再開後の unconnected fd 76 は inode 値が伝達されていないため、metadata に未採取と記録した。udp6 は対象の connected 行なしという観測のみで、全出力を保存していない。

parser と状態遷移の自動テストがこの抜粋を使用する。fixture による合格は、自動検出や OSC 送信の実機合格を意味しない。
