from manim import *


class SuppressedScene(Scene):
    def construct(self):
        title = Text("Title", font="Comic Sans MS")  # manim-lint: ignore[MLD306]
        self.add(title)
