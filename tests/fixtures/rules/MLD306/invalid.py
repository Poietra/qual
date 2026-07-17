from manim import *


class FontScene(Scene):
    def construct(self):
        title = Text("Title", font="Comic Sans MS")
        body = MarkupText("Body", font="Papyrus")
        self.add(title, body)
