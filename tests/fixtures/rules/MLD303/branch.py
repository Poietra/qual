from manim import *


def pick(flag):
    if flag:
        return "C:\\windows\\a.svg"
    return "b.svg"


class BranchPathScene(Scene):
    def construct(self):
        # The path is not a literal at the construction site: the rule
        # cannot prove foreign syntax and stays silent.
        self.add(SVGMobject(pick(False)))
