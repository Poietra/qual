from manim import *


def pick(flag):
    if flag:
        return "Logo.svg"
    return "logo.svg"


class BranchCaseScene(Scene):
    def construct(self):
        # The path is not a literal at the construction site: the rule
        # cannot verify the on-disk case and stays silent.
        self.add(SVGMobject(pick(True)))
