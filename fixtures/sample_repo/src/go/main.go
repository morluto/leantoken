package main

import (
    "fmt"
    "leantoken-fixture/point"
)

func main() {
    p := point.Point{X: 1, Y: 2}
    q := point.Point{X: 0, Y: 0}
    fmt.Println(p.Distance(q))
}
