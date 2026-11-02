// Package words counts and deduplicates words.
package words

import "sort"

// Unique returns the distinct words, sorted.
func Unique(words []string) []string {
	seen := map[string]bool{}
	var out []string
	for _, w := range words {
		if !seen[w] {
			seen[w] = true
			out = append(out, w)
		}
	}
	sort.Strings(out)
	return out
}

// Counts returns how often each word occurs.
func Counts(words []string) map[string]int {
	counts := map[string]int{}
	for _, w := range words {
		counts[w]++
	}
	return counts
}
