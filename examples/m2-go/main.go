// Command words prints the distinct words of its standard input.
package main

import (
	"bufio"
	"fmt"
	"os"

	"example.com/words/words"
)

func main() {
	var in []string
	scanner := bufio.NewScanner(os.Stdin)
	scanner.Split(bufio.ScanWords)
	for scanner.Scan() {
		in = append(in, scanner.Text())
	}
	for _, w := range words.Unique(in) {
		fmt.Println(w)
	}
}
