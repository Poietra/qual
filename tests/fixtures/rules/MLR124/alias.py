import manim as mn
from manim import Text as Label


class Alias(mn.Scene):
    def construct(self):
        a = mn.Text("<b>bold</b>")
        b = Label("<u>underline</u>")
