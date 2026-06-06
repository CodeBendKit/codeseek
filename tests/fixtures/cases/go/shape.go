package main

// Shape represents a geometric shape
type Shape struct {
	name string
}

func (s Shape) Area() int {
	return 0
}

// Rectangle is a shape with width and height
type Rectangle struct {
	width  int
	height int
}

func (r Rectangle) Area() int {
	return r.width * r.height
}
