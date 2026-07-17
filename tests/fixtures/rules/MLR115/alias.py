import manim as mn
from manim import Text as Label


class Alias(mn.Scene):
    def construct(self):
        a = mn.Text("hello", font_size=0)
        b = Label("world", font_size=-3)
