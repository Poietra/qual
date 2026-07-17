from manim import Circle, Scene, Square


class StraightLine(Scene):
    def construct(self):
        sq = Square()
        circle = Circle()
        self.add(sq)
        self.add(circle)
        self.add(sq)
        self.remove(circle)
