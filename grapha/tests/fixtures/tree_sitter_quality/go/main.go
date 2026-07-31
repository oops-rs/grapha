package fixture

import helper "example.com/tree-sitter-quality/go/helper"

/* GoWorker coordinates the Go fixture. */
type GoWorker struct {
	onReady func()
	label   string
}

func (w *GoWorker) Run() {
	label := helper.FormatLabel("go")
	w.onReady()
	_ = label
}

func NewGoWorker() *GoWorker {
	return &GoWorker{
		onReady: reportReady,
		label:   helper.FormatLabel("go"),
	}
}

func reportReady() {}

func init() {
	NewGoWorker().Run()
}
