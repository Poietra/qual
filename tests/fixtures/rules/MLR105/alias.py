import manim as mn
from manim import MarkupText as MT


class Alias(mn.Scene):
    def construct(self):
        one = mn.MarkupText("<b>bold</i>")
        two = MT("<u>never closed")
