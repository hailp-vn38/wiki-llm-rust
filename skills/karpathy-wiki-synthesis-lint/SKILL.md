---
name: karpathy-wiki-synthesis-lint
description: "Post-ingest workflow for cross-video compounding, deduplication, filing syntheses, and running Karpathy Wiki Lint."
---

# Karpathy Wiki Synthesis & Lint Skill

After all videos in the playlist have been ingested by subagents, execute this final compounding workflow:

## Step 1: Audit & Consolidate the Wiki (`wiki_tree` + `wiki_get`)
1. Call `wiki_get(file="wiki/index.md")` and `wiki_tree(scope="wiki")`.
2. Verify that all video pages in `wiki/sources/youtube/` are present and richly cross-linked in logical progression (e.g., Part 1 coding -> Part 2 multi-symbol OOP -> 100% Tick Data backtest -> Bugfixes & ATR filter).
3. If any two `entities/` or `concepts/` pages are redundant duplicates, merge the content into the canonical page via `wiki_write_file`, update links via `wiki_patch_file`, and remove the duplicate file via `wiki_delete_file`.

## Step 2: File the Master Playlist Synthesis (`wiki_file_synthesis`)
Call `wiki_file_synthesis` to create `wiki/syntheses/<topic-slug>.md` containing:
1. **Luận điểm cốt lõi & Tiến trình tiến hóa của Chiến lược**: Phân tích sâu cách kiến trúc hệ thống phát triển qua từng video (từ ý tưởng gốc -> lập trình đơn tài sản -> mở rộng hướng đối tượng đa tài sản -> kiểm chứng dữ liệu Tick 10 năm -> phát hiện bug `Lots`/`days_back` và tích hợp bộ lọc `ATR`).
2. **Bảng So sánh Chi tiết giữa các Video**: Bảng Markdown đối chiếu từng video (`Video`, `Mục tiêu kỹ thuật`, `Cấu trúc Code / Chỉ báo`, `Kết quả Backtest & Rủi ro`, `Link Wiki & Raw Source`).
3. **Tổng hợp các Thực thể (`entities/`) & Khái niệm (`concepts/`) Cốt lõi**: Liên kết `[[entities/<slug>|...]]` và `[[concepts/<slug>|...]]` tới tất cả các trang thực thể và khái niệm trong Wiki.
4. **Sơ đồ Kiến trúc & Tiến trình Tri thức (`flowchart LR`)**: Sơ đồ Mermaid thể hiện mối quan hệ nhân quả giữa các video và các thành phần kỹ thuật.

## Step 3: Back-link Synthesis & Run `wiki_lint`
1. Patch at least one video page (or all video pages) in `wiki/sources/youtube/` using `wiki_patch_file` so they link to `[[wiki/syntheses/<topic-slug>|<Title>]]` (ensuring the synthesis page has inbound links and is not an orphan).
2. Call `wiki_lint` to run Karpathy's health-check (`uningested_sources`, `broken_links`, `orphan_wiki_pages`, `stale_wiki_pages`).
3. If `wiki_lint` reports ANY `broken_links` or `orphan_wiki_pages`, fix them immediately using `wiki_write_file`, `wiki_patch_file`, or `wiki_delete_file`, and call `wiki_lint` again until `is_healthy: true`!

