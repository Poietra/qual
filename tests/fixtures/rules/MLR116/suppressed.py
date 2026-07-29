from manim import *


class SuppressedScene(Scene):
    def construct(self):
        path = VMobject()
        path.add_line_to([1, 0, 0])  # qual: ignore[MLR116]
        self.add(path)
