import Foundation

/// 命令面板的模糊匹配：子序列 + 简单打分，不引入依赖。
///
/// 分数越低越好。空查询视为匹配（分数 0）。大小写不敏感。
enum CommandPaletteFuzzy {
    struct Candidate<Item: Sendable>: Sendable {
        let item: Item
        let text: [Character]
    }

    static func normalize(_ text: String) -> [Character] {
        Array(text.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: nil))
    }

    static func prepare<Item: Sendable>(_ items: [Item], text: (Item) -> String,
                                        normalize: (String) -> [Character] = normalize) -> [Candidate<Item>] {
        items.map { (item: $0, text: text($0)) }
            .sorted { $0.text.localizedStandardCompare($1.text) == .orderedAscending }
            .map { Candidate(item: $0.item, text: normalize($0.text)) }
    }

    /// 若 `query` 的字符能按顺序出现在 `text` 中则返回分数，否则 nil。
    /// 连续命中、靠前命中、目标更短时分数更低。
    static func score(query: String, in text: String) -> Int? {
        score(needle: normalize(query), hay: normalize(text))
    }

    static func score(needle: [Character], hay: [Character]) -> Int? {
        if needle.isEmpty { return 0 }
        var hi = 0
        var score = 0
        var consecutive = 0
        var first = -1
        for ch in needle {
            var found = false
            while hi < hay.count {
                if hay[hi] == ch {
                    if first < 0 { first = hi }
                    // 连续命中加分（减分值），跳过的字符越多越差。
                    if consecutive > 0 { score -= 2 }
                    score += hi
                    consecutive += 1
                    hi += 1
                    found = true
                    break
                }
                consecutive = 0
                hi += 1
            }
            if !found { return nil }
        }
        // 更短的目标、更靠前的起点更好。
        score += hay.count + first
        return score
    }

    /// 按分数升序；同分按标题本地化比较。
    static func ranked<Item: Sendable>(_ candidates: [Candidate<Item>], needle: [Character]) -> [(item: Item, score: Int)] {
        var buckets: [Int: [Item]] = [:]
        for candidate in candidates {
            if let score = score(needle: needle, hay: candidate.text) {
                buckets[score, default: []].append(candidate.item)
            }
        }
        return buckets.keys.sorted().flatMap { score in
            buckets[score]!.map { (item: $0, score: score) }
        }
    }

    static func ranked<Item: Sendable>(_ items: [Item], query: String, text: (Item) -> String) -> [(item: Item, score: Int)] {
        ranked(prepare(items, text: text), needle: normalize(query.trimmingCharacters(in: .whitespacesAndNewlines)))
    }
}
