from manim.mobject.text.text_mobject import register_font

from manim import *


class Bad(Scene):
    def construct(self):
        register_font("fonts/custom.ttf")
        title = Text("hello", font="Custom Font")
        self.add(title)


register_font("module_level.ttf")
register_font("suppressed.ttf")  # manim-lint: ignore[MLR117]
