from manim import Scene, Square


class Loops(Scene):
    def construct(self):
        for i in range(3):
            sq = Square()
            self.add(sq)
