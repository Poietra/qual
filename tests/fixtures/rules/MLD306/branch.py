from manim import *


def pick(flag):
    if flag:
        return "Comic Sans MS"
    return "Noto Sans"


class BranchFontScene(Scene):
    def construct(self):
        # The font is not a literal at the construction site: silent.
        self.add(Text("Title", font=pick(True)))
