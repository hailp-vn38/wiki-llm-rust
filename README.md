# `llm-wiki` (Rust)

Thư viện Rust local-first kết hợp kiến trúc **LLM Wiki** của Andrej Karpathy ([Gist `442a6bf555914893e9891c11519de94f`](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)) với động cơ xử lý & tìm kiếm **QMD (Query Markup Documents)** ([`tobi/qmd`](https://github.com/tobi/qmd)), tương thích 100% với **Obsidian's Graph View**.

## Tính Năng Nổi Bật

1. **Logic xử lý & tìm kiếm QMD**:
   - **Smart Markdown Chunking**: Cắt chunk (~900 tokens, 15% overlap) theo điểm ngắt ngữ nghĩa (Heading H1–H6, Code Fence Protection chống đứt khối code ` ``` `, Horizontal Rule, Paragraph) kèm công thức suy giảm khoảng cách bình phương.
   - **Hybrid Search Pipeline**: Kết hợp **SQLite FTS5 (BM25)** + **Vector Search** (`vec_cosine_distance`) + **Reciprocal Rank Fusion (RRF, $k=60$)** (trọng số $2\times$ cho truy vấn đầu, thưởng hạng Top-1 $+0.05$, Top-2/3 $+0.02$) + **Position-Aware Reranker Blending**.
   - **Metadata AST Filtering & Discovery**: Đánh chỉ mục YAML frontmatter và lọc đệ quy theo biểu thức logic (`and`, `or`, `not`, `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `in`, `nin`, `all`, `contains`, `prefix`, `suffix`, `exists`, `type`).
2. **Tương thích Obsidian's Graph View**:
   - Thư mục dữ liệu gốc (`data_root`) mở trực tiếp được bằng **Obsidian** như một Vault hoàn chỉnh (tự động tạo `.obsidian/graph.json` phân màu cho `sources`, `wiki/sources`, `wiki/entities`, `wiki/concepts`, `wiki/syntheses`).
   - Hỗ trợ đầy đủ `[[WikiLinks]]`, `[[path#Heading|Alias]]`, `![[Embeds]]`, liên kết trong YAML Frontmatter (`source_file: "[[sources/...|...]]"`), và phân giải liên kết theo đúng thứ tự ưu tiên của Obsidian.
3. **Chỉ cần cấu hình duy nhất 1 thư mục (`WikiEngine::open(data_root)`)**:
   - Tự động khởi tạo `sources/`, `wiki/`, `SCHEMA.md`, `wiki/index.md`, `wiki/log.md` và cơ sở dữ liệu `.wiki/index.sqlite`.
4. **Tự động tạo thư mục lồng nhau khi tạo file**:
   - Khi truyền đường dẫn như `"docs/flow/lib.md"`, thư viện tự động kiểm tra chống Path Traversal và tạo các folder cha `docs/` -> `docs/flow/` rồi ghi `lib.md`.
5. **Giữ nguyên tên file Source khi tạo Wiki (`Ingest`)**:
   - Khi ingest `sources/docs/flow/lib.md`, trang tóm tắt trong `wiki/` được tạo tại `wiki/sources/docs/flow/lib.md` (giữ nguyên tên file `lib.md`, tuyệt đối không sinh ID ngẫu nhiên làm tên file).
6. **API lấy cây thư mục (`get_tree`)**:
   - Lấy cây thư mục đệ quy (`FileTreeNode` + `.to_ascii_tree()`) cho `sources`, `wiki`, toàn bộ vault, hoặc bất kỳ đường dẫn con nào.
7. **Bộ 15 LLM Tools dựng sẵn & Thực thi Thích ứng theo Context Window (`AdaptiveReadBudget`)**:
   - Cung cấp JSON Schema (`llm_tool_definitions()`), hàm phân loại `is_read_only_wiki_tool()`, hàm thực thi JSON (`execute_llm_tool()`) và hàm thực thi tối ưu token theo `context_limit_tokens` (`execute_llm_tool_with_budget()`).
   - Tích hợp cơ chế đọc phân trang theo dòng của `javis-os` (`slice_lines_with_budget`), danh mục gọn `read_compact_index_md` (kèm `aliases` chống phân mảnh trang), và tự động sửa lỗi thẻ đóng `</mermaid>`.
8. **Tiện ích Mở rộng Flow đa nguồn (`write_topic_source`, `apply_template`, `build_next_autoflow_prompt`)**:
   - Hỗ trợ sẵn chuẩn hóa tiếng Việt (`fold_vietnamese`, `slugify`, `sanitize_topic_folder`), chuyển đổi phụ đề `vtt_to_text`, lưu Raw Source theo chủ đề `<domain>/<topic>` (`write_topic_source`), bộ System Prompts đa tác tử (`ROOT_DISPATCHER_PROMPT`, `SOURCE_COLLECTOR_PROMPT`, `WIKI_INGESTOR_PROMPT`, `WIKI_CURATOR_PROMPT`) và hàm lập kế hoạch tự động `build_next_autoflow_prompt`.

## Chạy Kiểm Thử & Ví Dụ

```bash
# Chạy toàn bộ test suite
cargo test

# Chạy ví dụ minh họa toàn bộ luồng hoạt động cơ bản
cargo run --example full_workflow

# Chạy ví dụ Live YouTube Playlist -> Raw Source Markdown -> Karpathy LLM Wiki (dùng orchestrator-agent)
cp .env.live.example .env.live   # điền G0_OPENAI_BASE_URL, G0_OPENAI_MODEL, G0_OPENAI_API_KEY
cargo run --example youtube_playlist_wiki -- "<PLAYLIST_URL>" --limit 3 --lang en,vi
```

Xem chi tiết thiết kế kiến trúc và luồng xử lý tại:
- [ARCHITECTURE.md](./ARCHITECTURE.md) — Đặc tả kỹ thuật, SQLite schema, thuật toán QMD & Rust API.
- [docs/karpathy-llm-wiki.md](./docs/karpathy-llm-wiki.md) — Nguyên bản ý tưởng "LLM Wiki" của Andrej Karpathy và bảng ánh xạ vào `llm-wiki-rs`.
- [examples/youtube_playlist_wiki/](./examples/youtube_playlist_wiki/main.rs) — Ứng dụng mẫu tích hợp [`orchestrator-agent`](https://crates.io/crates/orchestrator-agent) để xử lý YouTube Playlist thành Raw Sources (`sources/youtube/`) và tự động tổng hợp, liên kết chéo `[[WikiLinks]]`, kiểm tra sức khỏe (`lint_wiki`) trong `wiki/`.
