from manim import *


class Good(Scene):
    def construct(self, msg):
        ok = MarkupText("<b>ok</b>")
        nested = MarkupText("<b><i>both</i></b>")
        extension = MarkupText("<gradient from='RED' to='BLUE'>x</gradient>")
        entities = MarkupText("a &amp; b &#38; c &#x26; d")
        comparison = MarkupText("x < y")
        dynamic = MarkupText(msg)
        formatted = MarkupText(f"<b>{msg}</i>")
