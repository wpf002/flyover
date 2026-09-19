package main

import (
	"fmt"
	"strings"
)

type Greeter struct {
	name string
}

func (g Greeter) Hello() string {
	return "hi " + g.name
}

func main() {
	fmt.Println(strings.ToUpper("x"))
}
