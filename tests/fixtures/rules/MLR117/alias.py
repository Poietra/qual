import manim.mobject.text.text_mobject as text_module
from manim.mobject.text.text_mobject import register_font as rf


def load_fonts():
    text_module.register_font("a.ttf")
    rf("b.ttf")
