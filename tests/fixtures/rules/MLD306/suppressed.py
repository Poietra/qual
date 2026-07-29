from manim import *


class SuppressedScene(Scene):
    def construct(self):
        title = Text("Title", font="Comic Sans MS")  # qual: ignore[MLD306]
        self.add(title)
