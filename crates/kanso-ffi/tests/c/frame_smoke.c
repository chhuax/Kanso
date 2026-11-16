/* C 侧冒烟测试：只通过头文件和三个导出函数读取 mock 帧。成功返回 0。 */
#include <stdio.h>
#include <string.h>

#include "kanso_frame.h"

#define CHECK(cond)                                                     \
    do {                                                                \
        if (!(cond)) {                                                  \
            fprintf(stderr, "检查失败 %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            return 1;                                                   \
        }                                                               \
    } while (0)

int main(void) {
    KansoFrame *frame = kanso_mock_frame_create(80, 24, 1);
    CHECK(frame != NULL);
    const KansoFrameHeader *h = kanso_frame_header(frame);
    CHECK(h != NULL);
    CHECK(((uintptr_t)h % 8) == 0);
    CHECK(h->magic == KANSO_FRAME_MAGIC);
    CHECK(h->version == KANSO_FRAME_VERSION);
    CHECK(h->header_size == sizeof(KansoFrameHeader));
    CHECK(h->cols == 80 && h->rows == 24 && h->seq == 1);
    CHECK(h->line_meta_offset == h->header_size);
    CHECK(h->cells_offset == h->line_meta_offset + 24u * sizeof(KansoLineMeta));
    CHECK(h->extras_offset == h->cells_offset + 80u * 24u * sizeof(KansoCell));
    CHECK(h->pool_offset == h->extras_offset + h->extras_count * sizeof(KansoCellExtra));
    CHECK(h->pool_offset + h->pool_len <= h->total_len);
    CHECK(h->total_len % 8 == 0);

    int saw_grapheme = 0, saw_wide = 0, saw_prompt = 0, saw_blank = 0;
    for (uint16_t r = 0; r < h->rows; r++) {
        const KansoLineMeta *m = kanso_frame_line_meta(h, r);
        CHECK(m->abs_line == h->first_abs_line + r);
        saw_prompt |= (m->flags & KANSO_LINE_PROMPT) != 0;
        saw_blank |= (m->flags & KANSO_LINE_BLANK) != 0;
        const KansoCell *row = kanso_frame_row(h, r);
        for (uint16_t c = 0; c < h->cols; c++) {
            const KansoCell *cell = &row[c];
            CHECK(cell->extra <= h->extras_count);
            saw_wide |= (cell->flags & KANSO_CELL_WIDE) != 0;
            const KansoCellExtra *e = kanso_frame_extra(h, cell);
            if (e != NULL && e->grapheme_len != 0) {
                CHECK((uint64_t)e->grapheme_offset + e->grapheme_len <= h->pool_len);
                const char *g = kanso_frame_grapheme(h, e);
                CHECK(g != NULL);
                /* 字形簇至少包含两个码点，所以字节数大于首码点占用的字节数。 */
                CHECK(e->grapheme_len >= 2);
                saw_grapheme = 1;
            }
        }
    }
    for (uint32_t i = 0; i < h->extras_count; i++) {
        const KansoCellExtra *e = (const KansoCellExtra *)(kanso_frame_base(h) + h->extras_offset) + i;
        CHECK((uint64_t)e->grapheme_offset + e->grapheme_len <= h->pool_len);
    }
    CHECK(saw_grapheme && saw_wide && saw_prompt && saw_blank);

    /* seq=1 时第 0 行是脚本第 1 行（索引色），绝对行号 1 不是首个提示符。 */
    CHECK((kanso_frame_line_meta(h, 0)->flags & KANSO_LINE_FIRST_PROMPT) == 0);
    CHECK(kanso_color_tag(kanso_frame_row(h, 0)[12].fg) == KANSO_COLOR_TAG_INDEXED);
    CHECK(kanso_color_payload(kanso_frame_row(h, 0)[12].fg) == 4);

    kanso_frame_destroy(frame);
    kanso_frame_destroy(NULL);
    CHECK(kanso_frame_header(NULL) == NULL);
    puts("frame_smoke ok");
    return 0;
}
