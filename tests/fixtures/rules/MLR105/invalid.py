from manim import *


class Bad(Scene):
    def construct(self):
        mismatched = MarkupText("<b>bold</i>")
        unclosed = MarkupText("<u>never closed")
        entity = MarkupText("x &foo; y")
        stray = MarkupText("stray </b> here")
        supp = MarkupText("<b>oops</i>")  # manim-lint: ignore[MLR105]
