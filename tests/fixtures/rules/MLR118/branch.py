import manim as mn


def choose(flag):
    if flag:
        return "assets/unsupported.svg"
    return "assets/valid.svg"


class DynamicAsset(mn.Scene):
    def construct(self):
        self.add(mn.SVGMobject(choose(True)))
