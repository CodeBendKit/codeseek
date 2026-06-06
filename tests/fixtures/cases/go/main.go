package main

import (
	"fmt"
)

type Point struct {
	X int
	Y int
}

func NewPoint(x int, y int) Point {
	return Point{X: x, Y: y}
}

func (p *Point) Move(dx int, dy int) {
	p.X += dx
	p.Y += dy
}

func main() {
	p := NewPoint(1, 2)
	p.Move(3, 4)
	fmt.Println(p.X, p.Y)
}
