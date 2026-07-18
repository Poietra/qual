from manim import *


class Good(Scene):
    def construct(self, message):
        a = Text("a < b and b > c")
        b = Text("<html>not pango</html>")
        c = Text("<b>unclosed has no matching pair")
        d = MarkupText("<b>bold</b>")
        e = Text(message)
        f = Text(f"<b>{message}</b>")
